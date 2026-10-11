// Builds dshell's chrome UI into `dist/`, which Tauri embeds into the binary.
//
// `web/index.html` is the tab-strip UI (the only webview with IPC access).
// `web/shell/*.html` are the loading and error pages a content webview shows
// before (or instead of) a DSH page. They are separate documents on purpose: the
// error page must be able to `invoke` back into dshell, and only a
// dshell-origin document can.

import { build } from 'esbuild';
import { cp, mkdir, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const webDir = path.join(root, 'web');
const outDir = path.join(root, 'dist');
const dev = process.argv.includes('--dev');

const entryPoints = {
  app: path.join(webDir, 'app.ts'),
  shell: path.join(webDir, 'shell.ts'),
};

/** Rewrite the `__PLACEHOLDER__` tokens the HTML sources carry. */
async function rewriteHtml() {
  const appHtmlPath = path.join(outDir, 'index.html');
  const appHtml = await readFile(appHtmlPath, 'utf8');
  await writeFile(
    appHtmlPath,
    appHtml.replaceAll('__APP_JS__', './app.js').replaceAll('__STYLES__', './styles.css'),
  );

  const shellDir = path.join(outDir, 'shell');
  for (const file of await readdir(shellDir)) {
    if (!file.endsWith('.html')) continue;
    const full = path.join(shellDir, file);
    const html = await readFile(full, 'utf8');
    const next = html
      .replaceAll('__SHELL_JS__', '../shell.js')
      .replaceAll('__STYLES__', '../styles.css');
    if (next !== html) await writeFile(full, next);
  }
}

async function copyStatic() {
  await cp(path.join(webDir, 'index.html'), path.join(outDir, 'index.html'));
  await cp(path.join(webDir, 'shell'), path.join(outDir, 'shell'), { recursive: true });
  await cp(path.join(webDir, 'styles.css'), path.join(outDir, 'styles.css'));
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
    // The IPC object is injected by the webview at runtime.
    logLevel: 'info',
  });
  await copyStatic();
  await rewriteHtml();
  console.log(`[dshell] chrome UI built into dist/ (${dev ? 'dev' : 'release'})`);
}

main().catch((error) => {
  console.error('[dshell] UI build failed:', error);
  process.exit(1);
});
