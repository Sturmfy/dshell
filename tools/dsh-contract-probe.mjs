// DSH contract regression probe.
//
// dshell sits on top of a *developer preview* tool whose flags and stdout
// protocol can change without notice. This script asserts the handful of
// behaviours dshell's correctness depends on, so a breaking upstream release is
// discovered here instead of as a blank window:
//
//   1. `web --help` still advertises the flags dshell passes;
//   2. stdout still carries the exact `dsh web: <url>` readiness line;
//   3. that URL still needs a token, and the token still exchanges for a cookie;
//   4. `/` still returns 200 with the `__DSH_BOOT__` marker (what dshell uses to
//      distinguish "ready" from "frontend bundle missing, server up anyway");
//   5. an unauthenticated `GET /` still answers 401, which is the positive
//      signal dshell uses to recognise an existing instance.
//
// Usage:
//   node tools/dsh-contract-probe.mjs --dsh <path-to/lib/bin.js> [--home <dir>] [--work <dir>]
//
// Exits 0 when every assertion holds, 1 otherwise. No dependencies.

import { spawn } from 'node:child_process';
import { mkdtempSync, mkdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

const args = process.argv.slice(2);
function arg(name, fallback) {
  const index = args.indexOf(`--${name}`);
  return index >= 0 && args[index + 1] ? args[index + 1] : fallback;
}

const dshEntry = arg('dsh', path.join('node_modules', '@deepseek-ai', 'dsh', 'lib', 'bin.js'));
const home = arg('home', mkdtempSync(path.join(tmpdir(), 'dsh-probe-home-')));
const work = arg('work', mkdtempSync(path.join(tmpdir(), 'dsh-probe-work-')));
const nodeBin = arg('node', process.execPath);

mkdirSync(home, { recursive: true });
mkdirSync(work, { recursive: true });

const results = [];
function check(name, ok, detail = '') {
  results.push({ name, ok, detail });
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? `  — ${detail}` : ''}`);
}

function run(cmd, cmdArgs, options = {}) {
  return new Promise((resolve) => {
    const child = spawn(cmd, cmdArgs, {
      stdio: ['ignore', 'pipe', 'pipe'],
      env: { ...process.env, NO_COLOR: '1', DSH_HOME: home, ...options.env },
      cwd: options.cwd ?? work,
      shell: false,
    });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (chunk) => (stdout += chunk));
    child.stderr.on('data', (chunk) => (stderr += chunk));
    child.on('error', (error) => resolve({ code: -1, stdout, stderr: String(error) }));
    child.on('exit', (code) => resolve({ code, stdout, stderr }));
  });
}

console.log(`dsh entry : ${dshEntry}`);
console.log(`DSH_HOME  : ${home}`);
console.log(`workspace : ${work}\n`);

// 1. capability probe -------------------------------------------------------
const helpResult = await run(nodeBin, [dshEntry, 'web', '--help']);
const helpText = `${helpResult.stdout}\n${helpResult.stderr}`;
for (const flag of ['--no-open', '--port', '--host', '--trusted-host']) {
  check(`web --help advertises ${flag}`, helpText.includes(flag));
}

// 2..4 live server ----------------------------------------------------------
const child = spawn(nodeBin, [dshEntry, 'web', '--no-open', '--port', '0'], {
  stdio: ['ignore', 'pipe', 'pipe'],
  env: { ...process.env, NO_COLOR: '1', DSH_HOME: home },
  cwd: work,
});

const READY = /^dsh web: (https?:\/\/\S+)$/;
const launchUrl = await new Promise((resolve) => {
  let buffer = '';
  const timer = setTimeout(() => resolve(null), 120000);
  const onLine = (chunk) => {
    buffer += chunk;
    let index;
    while ((index = buffer.indexOf('\n')) >= 0) {
      const line = buffer.slice(0, index).replace(/\r$/, '');
      buffer = buffer.slice(index + 1);
      const match = READY.exec(line.trim());
      if (match) {
        clearTimeout(timer);
        resolve(match[1]);
      }
    }
  };
  child.stdout.on('data', onLine);
  child.stderr.on('data', onLine);
  child.on('exit', () => {
    clearTimeout(timer);
    resolve(null);
  });
});

check('stdout carries the `dsh web: <url>` readiness line', Boolean(launchUrl), launchUrl ?? 'not seen');

if (launchUrl) {
  const parsed = new URL(launchUrl);
  const origin = `${parsed.protocol}//${parsed.host}`;
  check('readiness URL carries a token', parsed.searchParams.has('token'));
  check('readiness URL points at a loopback host', ['127.0.0.1', 'localhost', '::1'].includes(parsed.hostname));

  // (5) unauthenticated GET / must be 401 — the "this really is DSH" signal.
  const bare = await fetch(`${origin}/`, { redirect: 'manual' });
  check('unauthenticated GET / answers 401', bare.status === 401, `status ${bare.status}`);

  // (3) token exchange: 303 + Set-Cookie
  const exchange = await fetch(launchUrl, { redirect: 'manual' });
  const setCookie = exchange.headers.get('set-cookie') ?? '';
  check('token exchange answers 303', exchange.status === 303, `status ${exchange.status}`);
  check(
    'token exchange sets a dsh-auth cookie (HttpOnly; SameSite=Strict)',
    /dsh-auth-[A-Za-z0-9_-]+=/.test(setCookie) &&
      /HttpOnly/i.test(setCookie) &&
      /SameSite=Strict/i.test(setCookie),
  );

  const cookie = setCookie.split(';')[0];
  const location = exchange.headers.get('location') ?? '/';
  const followUrl = location.startsWith('http') ? location : `${origin}/${location.replace(/^\.?\//, '')}`;

  // (4) authenticated GET / must show the boot marker.
  const page = await fetch(followUrl, { headers: { cookie }, redirect: 'manual' });
  const body = await page.text();
  check('authenticated GET / answers 200', page.status === 200, `status ${page.status}`);
  check('page contains the __DSH_BOOT__ marker', body.includes('__DSH_BOOT__'));

  // A missing frontend bundle is a 404 with an empty stderr — the "false ready"
  // case dshell's step-2 verification exists to catch.
  check(
    'root document is served over plain HTTP (no TLS flags in this release)',
    parsed.protocol === 'http:',
    parsed.protocol,
  );
}

// Clean up the whole process tree.
try {
  child.kill();
} catch {
  /* already gone */
}
await new Promise((resolve) => setTimeout(resolve, 1500));

const failed = results.filter((result) => !result.ok);
console.log(`\n${results.length - failed.length}/${results.length} checks passed`);
if (failed.length > 0) {
  console.log('FAILED:');
  for (const result of failed) console.log(`  - ${result.name}${result.detail ? ` (${result.detail})` : ''}`);
  process.exit(1);
}
