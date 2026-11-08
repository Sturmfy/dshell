// Keeps every place dshell spells its version in step.
//
// `package.json` is the source of truth. `npm version <bump>` runs this as its
// `version` lifecycle script, so one command bumps all four files, commits them
// and creates the `vX.Y.Z` tag. It is also usable on its own:
//
//   node tools/set-version.mjs 0.2.0
//
// The other three are not redundant: `tauri.conf.json` is what the bundler
// stamps into the macOS bundle, `Cargo.toml` is what the binary reports, and
// `Cargo.lock` is what `--locked` builds would otherwise fight over.
//
// `tools/check-version.mjs` is the read-only half; this file never validates a
// tag, because `npm version` runs before there is one.

import { readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

/** SemVer 2.0.0, prerelease allowed, build metadata deliberately not. */
export const VERSION_RE = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/;

/** The files that carry the version, relative to the repository root. */
export const VERSION_FILES = [
  'package.json',
  'src-tauri/tauri.conf.json',
  'src-tauri/Cargo.toml',
  'src-tauri/Cargo.lock',
];

/** Rewrite the `[package]` version of a Cargo.toml, leaving everything else. */
function bumpCargoToml(text, version) {
  const newline = text.includes('\r\n') ? '\r\n' : '\n';
  const lines = text.split(/\r?\n/);
  const next = [];
  let inPackage = false;
  let replaced = false;
  for (const line of lines) {
    const trimmed = line.trim();
    if (/^\[.+\]$/.test(trimmed)) {
      inPackage = trimmed === '[package]';
      next.push(line);
      continue;
    }
    if (inPackage && !replaced && /^version\s*=/.test(trimmed)) {
      next.push(`version = "${version}"`);
      replaced = true;
      continue;
    }
    next.push(line);
  }
  if (!replaced) {
    throw new Error('src-tauri/Cargo.toml has no [package] version');
  }
  return next.join(newline);
}

/** Rewrite the `dshell` entry of Cargo.lock. */
function bumpCargoLock(text, version) {
  const re = /(\[\[package\]\]\r?\nname = "dshell"\r?\nversion = ")[^"]*(")/;
  if (!re.test(text)) {
    throw new Error('src-tauri/Cargo.lock has no `dshell` package entry');
  }
  return text.replace(re, `$1${version}$2`);
}

/** Rewrite the `version` of a JSON file, preserving order and indentation. */
function bumpJson(text, version) {
  const parsed = JSON.parse(text);
  parsed.version = version;
  const newline = text.endsWith('\n') ? '\n' : '';
  return JSON.stringify(parsed, null, 2) + newline;
}

/**
 * Apply `version` to every file under `root`.
 *
 * Returns `{ version, changed }`, where `changed` lists only the files whose
 * text actually moved — re-running an already-correct bump is a no-op.
 */
export async function setVersion({ root, version }) {
  if (!VERSION_RE.test(version)) {
    throw new Error(`"${version}" is not a SemVer version`);
  }

  const replacements = {
    'package.json': (text) => bumpJson(text, version),
    'src-tauri/tauri.conf.json': (text) => bumpJson(text, version),
    'src-tauri/Cargo.toml': (text) => bumpCargoToml(text, version),
    'src-tauri/Cargo.lock': (text) => bumpCargoLock(text, version),
  };

  const changed = [];
  for (const [relative, replace] of Object.entries(replacements)) {
    const file = path.join(root, relative);
    const before = await readFile(file, 'utf8');
    const after = replace(before);
    if (after === before) {
      continue;
    }
    await writeFile(file, after);
    changed.push(relative);
  }

  return { version, changed };
}

/** The CLI half: `node tools/set-version.mjs [version]`. */
async function main(argv) {
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
  // `npm version` exports the bumped value; a bare `node tools/set-version.mjs`
  // falls back to it too, which makes the script safe to run by hand after an
  // `npm version --no-git-tag-version`.
  const version = argv[2] ?? process.env.npm_package_version;
  if (!version) {
    console.error(
      '[dshell] set-version: give a version (`node tools/set-version.mjs 0.2.0`) ' +
        'or run it through `npm version`.',
    );
    process.exit(1);
  }

  try {
    const { changed } = await setVersion({ root, version });
    if (changed.length === 0) {
      console.log(`[dshell] version ${version} was already everywhere`);
      return;
    }
    console.log(`[dshell] version ${version}: updated ${changed.join(', ')}`);
  } catch (error) {
    console.error(`[dshell] set-version failed: ${error.message}`);
    process.exit(1);
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await main(process.argv);
}
