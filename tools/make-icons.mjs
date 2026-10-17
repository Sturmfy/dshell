// Packs the rasterised PNGs into `src-tauri/icons/icon.ico`.
//
//   node tools/make-icons.mjs
//
// The art lives in `icons/logo-tile.svg`; `tools/render-icons.mjs` turns it into
// `icons/png/{16,32,48,64,128,256}.png` (dev-only, needs a browser). This script
// is the build-time half: no browser, no drawing, just the ICO container, which
// is also why `npm run build` stays dependency-free.

import { copyFileSync, existsSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(HERE, '..');
const iconsDir = path.join(repo, 'src-tauri', 'icons');
const pngDir = path.join(iconsDir, 'png');

const sizes = [16, 32, 48, 64, 128, 256];

function ico(images) {
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(images.length, 4);

  const entries = [];
  let offset = 6 + images.length * 16;
  for (const image of images) {
    const entry = Buffer.alloc(16);
    entry[0] = image.size >= 256 ? 0 : image.size;
    entry[1] = image.size >= 256 ? 0 : image.size;
    entry[2] = 0;
    entry[3] = 0;
    entry.writeUInt16LE(1, 4);
    entry.writeUInt16LE(32, 6);
    entry.writeUInt32LE(image.data.length, 8);
    entry.writeUInt32LE(offset, 12);
    entries.push(entry);
    offset += image.data.length;
  }
  return Buffer.concat([header, ...entries, ...images.map((image) => image.data)]);
}

const images = sizes.map((size) => {
  const file = path.join(pngDir, `${size}.png`);
  if (!existsSync(file)) {
    console.error(
      `[icons] missing ${path.relative(repo, file)} — run \`node tools/render-icons.mjs\` first`,
    );
    process.exit(1);
  }
  const data = readFileSync(file);
  const width = data.readUInt32BE(16);
  const height = data.readUInt32BE(20);
  if (width !== size || height !== size) {
    console.error(`[icons] ${file} is ${width}x${height}, expected ${size}x${size}`);
    process.exit(1);
  }
  return { size, data };
});

writeFileSync(path.join(iconsDir, 'icon.ico'), ico(images));
copyFileSync(path.join(pngDir, '256.png'), path.join(iconsDir, 'icon.png'));
console.log(`[dshell] wrote src-tauri/icons/icon.ico (${sizes.join(', ')})`);
