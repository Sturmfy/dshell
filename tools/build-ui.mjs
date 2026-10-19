// Builds dshell's web UIs into `dist/`, which Tauri embeds into the binary.
//
// The documents, deliberately separate:
//
//   index.html            the tab strip (chrome webview) — the window's title bar
//   pages/*.html          dshell's own pages, each opened as a *tab*
//   shell/*.html          the loading and failure pages a DSH tab shows
//
// The strip is its own webview because it is the window's only drag region, so it
// must stay on screen no matter what a tab is showing. The pages are *not* a
// special region: they are tabs, rendered by the same content webview area a DSH
// page uses, which is what gives them the normal focus, shortcut and close rules.
//
// `pages/` and `shell/` are separate directories because they differ in one
// security-relevant way: a page document is served from dshell's own origin and
// may `invoke` dshell's commands, while a DSH page in the same region never can.

import { build } from 'esbuild';
import { existsSync } from 'node:fs';
import { cp, mkdir, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const webDir = path.join(root, 'web');
const outDir = path.join(root, 'dist');
const dev = process.argv.includes('--dev');

// Entry-point keys are the *output paths*, so each one must match the document
// that loads it: `pages/new.html` loads `./new.js`, therefore the key is
// `pages/new`. A mismatch here produces a page that renders an empty skeleton
// instead of failing loudly, so the two are kept in step by name.
const entryPoints = {
  strip: path.join(webDir, 'strip.ts'),
  shell: path.join(webDir, 'shell.ts'),
  menu: path.join(webDir, 'menu.ts'),
  'pages/new': path.join(webDir, 'pages', 'new.ts'),
  'pages/onboarding': path.join(webDir, 'pages', 'onboarding.ts'),
  'pages/settings': path.join(webDir, 'pages', 'settings.ts'),
};

/** Replace the `__TOKEN__` placeholders the HTML sources carry. */
async function rewriteHtml(file, replacements) {
  const html = await readFile(file, 'utf8');
  let next = html;
  for (const [token, value] of replacements) next = next.replaceAll(token, value);
  await writeFile(file, next);
}

/**
 * Copy the HTML documents next to their bundles, then fill in the tokens.
 *
 * Separate from the esbuild step because esbuild only knows about the entry
 * points; the HTML has to be copied *and* rewritten, and only `.html` — copying
 * the whole directory would ship the TypeScript sources too.
 */
async function copyDocs() {
  await cp(path.join(webDir, 'index.html'), path.join(outDir, 'index.html'));
  await cp(path.join(webDir, 'menu.html'), path.join(outDir, 'menu.html'));
  await cp(path.join(webDir, 'styles.css'), path.join(outDir, 'styles.css'));
  await cp(path.join(webDir, 'shell'), path.join(outDir, 'shell'), { recursive: true });

  const pagesOut = path.join(outDir, 'pages');
  await mkdir(pagesOut, { recursive: true });
  for (const file of await readdir(path.join(webDir, 'pages'))) {
    if (file.endsWith('.html')) {
      await cp(path.join(webDir, 'pages', file), path.join(pagesOut, file));
    }
  }
}

async function rewriteAll() {
  await rewriteHtml(path.join(outDir, 'index.html'), [
    ['__APP_JS__', './strip.js'],
    ['__STYLES__', './styles.css'],
  ]);
  for (const file of await readdir(path.join(outDir, 'pages'))) {
    if (!file.endsWith('.html')) continue;
    const bundle = file.replace(/\.html$/, '.js');
    // Pages live one level down, so the shared stylesheet is one level up while
    // their own bundle is a sibling.
    await rewriteHtml(path.join(outDir, 'pages', file), [
      ['__APP_JS__', `./${bundle}`],
      ['__STYLES__', '../styles.css'],
    ]);
  }
  for (const file of await readdir(path.join(outDir, 'shell'))) {
    if (!file.endsWith('.html')) continue;
    await rewriteHtml(path.join(outDir, 'shell', file), [
      ['__SHELL_JS__', '../shell.js'],
      ['__STYLES__', '../styles.css'],
    ]);
  }
}

/**
 * Fail the build when a document references a bundle that was not produced.
 *
 * This is not hypothetical tidiness: a page whose `<script src>` 404s still
 * renders its static markup, so the window shows a tab that looks alive with an
 * empty body and no error anywhere. That is exactly how a one-word filename
 * mismatch survives every Rust test, every type check and a clean clippy run.
 */
async function assertEveryBundleExists() {
  const referenced = [];
  for (const file of await readdir(path.join(outDir, 'pages'))) {
    if (!file.endsWith('.html')) continue;
    const html = await readFile(path.join(outDir, 'pages', file), 'utf8');
    for (const match of html.matchAll(/src="\.\/([\w.-]+\.js)"/g)) {
      referenced.push({ from: `pages/${file}`, bundle: match[1] });
    }
  }
  const html = await readFile(path.join(outDir, 'index.html'), 'utf8');
  for (const match of html.matchAll(/src="\.\/([\w.-]+\.js)"/g)) {
    referenced.push({ from: 'index.html', bundle: match[1] });
  }
  const menuHtml = await readFile(path.join(outDir, 'menu.html'), 'utf8');
  for (const match of menuHtml.matchAll(/src="\.\/([\w.-]+\.js)"/g)) {
    referenced.push({ from: 'menu.html', bundle: match[1] });
  }

  const missing = [];
  for (const { from, bundle } of referenced) {
    const target = from.startsWith('pages/')
      ? path.join(outDir, 'pages', bundle)
      : path.join(outDir, bundle);
    if (!existsSync(target)) missing.push(`${from} → ${bundle}`);
  }
  if (missing.length > 0) {
    throw new Error(
      `these documents point at bundles that were not built: ${missing.join(', ')}`,
    );
  }
}

async function main() {
  await rm(outDir, { recursive: true, force: true });
  await mkdir(outDir, { recursive: true });
  await build({
    entryPoints,
    outdir: outDir,
    bundle: true,
    format: 'esm',
    target: 'es2022',
    platform: 'browser',
    sourcemap: dev,
    minify: !dev,
    logLevel: 'info',
  });

  await copyDocs();
  await rewriteAll();
  await assertEveryBundleExists();
  console.log(`[dshell] web UIs built into dist/ (${dev ? 'dev' : 'release'})`);
}

main().catch((error) => {
  console.error('[dshell] UI build failed:', error);
  process.exit(1);
});
