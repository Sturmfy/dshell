// Rasterises `src-tauri/icons/logo-tile.svg` into the PNG sizes Windows wants.
//
//   node tools/render-icons.mjs
//
// Dev-only: needs a Chromium-based browser (Chrome or Edge). The generated PNGs
// are checked in, so `npm run build` never depends on a browser.
//
// Pipeline: logo-tile.svg → icons/png/{16..256}.png → make-icons.mjs packs
// them into icons/icon.ico (and copies 256 as icon.png).

import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(HERE, '..');
const iconsDir = path.join(repo, 'src-tauri', 'icons');
const tmpDir = path.join(iconsDir, '.render');
const pngDir = path.join(iconsDir, 'png');

const CANDIDATES = [
  process.env.CHROME,
  'C:/Program Files/Google/Chrome/Application/chrome.exe',
  'C:/Program Files (x86)/Google/Chrome/Application/chrome.exe',
  'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
  '/usr/bin/google-chrome',
  '/usr/bin/chromium',
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
].filter(Boolean);

const chrome = CANDIDATES.find((candidate) => existsSync(candidate));
if (!chrome) {
  console.error('[icons] no Chromium browser found; set CHROME=/path/to/chrome');
  process.exit(1);
}

const sizes = [16, 32, 48, 64, 128, 256];
mkdirSync(tmpDir, { recursive: true });
mkdirSync(pngDir, { recursive: true });

const fileUrl = (p) => `file:///${p.replace(/\\/g, '/')}`;

for (const size of sizes) {
  const html = `<!doctype html><html><body style="margin:0;background:transparent">
<img src="../logo-tile.svg" width="${size}" height="${size}" style="display:block">
</body></html>`;
  const page = path.join(tmpDir, `${size}.html`);
  writeFileSync(page, html);

  const out = path.join(pngDir, `${size}.png`);
  execFileSync(
    chrome,
    [
      '--headless=new',
      '--disable-gpu',
      '--hide-scrollbars',
      '--no-sandbox',
      '--force-device-scale-factor=1',
      `--window-size=${size},${size}`,
      '--default-background-color=00000000',
      `--screenshot=${out}`,
      fileUrl(page),
    ],
    { stdio: 'ignore' },
  );

  // Validate the raster actually has the requested dimensions.
  const png = readFileSync(out);
  const width = png.readUInt32BE(16);
  const height = png.readUInt32BE(20);
  const ok = width === size && height === size;
  console.log(`[icons] ${size}x${size} → ${ok ? 'ok' : `WRONG (${width}x${height})`}`);
  if (!ok) process.exit(1);
}

console.log(`[icons] wrote ${sizes.length} PNGs to src-tauri/icons/png`);
rmSync(tmpDir, { recursive: true, force: true });
