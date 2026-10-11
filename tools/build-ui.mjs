// Builds dshell's web UIs into `dist/`, which Tauri embeds into the binary.
//
// Three documents, deliberately separate:
//
//   index.html         the tab strip (chrome webview) — the window's title bar
//   panel.html         dshell's panels (panel webview) — *below* the strip
//   shell/*.html       the loading and failure pages a content webview shows
//
// The strip and the panel are separate webviews so the strip stays visible, and
// therefore draggable, while a panel is open. The shell pages are separate
// because they must be able to `invoke` back into dshell, and only a
// dshell-origin document can — a DSH page never can.

import { build } from 'esbuild';
import { cp, mkdir, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const webDir = path.join(root, 'web');
const outDir = path.join(root, 'dist');
const dev = process.argv.includes('--dev');

const entryPoints = {
  strip: path.join(webDir, 'strip.ts'),
  panel: path.join(webDir, 'panel.ts'),
  shell: path.join(webDir, 'shell.ts'),
};

/** Replace the `__TOKEN__` placeholders the HTML sources carry. */
async function rewriteHtml() {
  const replacements = [
    ['__APP_JS__', './strip.js'],
    ['__PANEL_JS__', './panel.js'],
    ['__SHELL_JS__', '../shell.js'],
    ['__STYLES__', './styles.css'],
  ];

  const rewrite = async (file) => {
    const html = await readFile(file, 'utf8');
    let next = html;
    for (const [token, value] of replacements) next = next.replaceAll(token, value);
    if (next !== html) await writeFile(file, next);
  };

  await rewrite(path.join(outDir, 'index.html'));
  await rewrite(path.join(outDir, 'panel.html'));
  for (const file of await readdir(path.join(outDir, 'shell'))) {
    if (file.endsWith('.html')) await rewrite(path.join(outDir, 'shell', file));
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

  await cp(path.join(webDir, 'index.html'), path.join(outDir, 'index.html'));
  await cp(path.join(webDir, 'panel.html'), path.join(outDir, 'panel.html'));
  await cp(path.join(webDir, 'shell'), path.join(outDir, 'shell'), { recursive: true });
  await cp(path.join(webDir, 'styles.css'), path.join(outDir, 'styles.css'));

  await rewriteHtml();
  console.log(`[dshell] web UIs built into dist/ (${dev ? 'dev' : 'release'})`);
}

main().catch((error) => {
  console.error('[dshell] UI build failed:', error);
  process.exit(1);
});
