// Generates `src-tauri/icons/icon.ico` (and a PNG) from scratch.
//
// Hand-writing the ICO container avoids depending on an image toolchain for a
// single asset. The mark is intentionally plain: dshell's own visual identity
// barely matters, because the content area is 100% DSH's own rendering.

import { mkdir, writeFile } from 'node:fs/promises';
import path from 'node:path';
import zlib from 'node:zlib';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const outDir = path.join(root, 'src-tauri', 'icons');

/** Raw RGBA pixels for one size: rounded square + three stacked tab bars. */
function render(size) {
  const pixels = Buffer.alloc(size * size * 4);
  const radius = size * 0.22;
  const bg = [0x2f, 0x6f, 0xed, 255];
  const strip = [0xff, 0xff, 0xff, 235];
  const accent = [0x9d, 0xc0, 0xff, 255];

  const inRounded = (x, y) => {
    const cx = Math.min(Math.max(x, radius), size - radius);
    const cy = Math.min(Math.max(y, radius), size - radius);
    const dx = x - cx;
    const dy = y - cy;
    return dx * dx + dy * dy <= radius * radius;
  };

  const rows = [
    { y0: 0.2, y1: 0.32, x0: 0.16, x1: 0.56, color: strip },
    { y0: 0.42, y1: 0.54, x0: 0.16, x1: 0.72, color: strip },
    { y0: 0.64, y1: 0.76, x0: 0.16, x1: 0.48, color: accent },
  ];

  for (let y = 0; y < size; y += 1) {
    for (let x = 0; x < size; x += 1) {
      const offset = (y * size + x) * 4;
      if (!inRounded(x + 0.5, y + 0.5)) {
        pixels[offset + 3] = 0;
        continue;
      }
      let color = bg;
      for (const row of rows) {
        const inRow =
          y / size >= row.y0 && y / size <= row.y1 && x / size >= row.x0 && x / size <= row.x1;
        if (inRow) {
          color = row.color;
          break;
        }
      }
      pixels[offset] = color[0];
      pixels[offset + 1] = color[1];
      pixels[offset + 2] = color[2];
      pixels[offset + 3] = color[3];
    }
  }
  return pixels;
}

function crc32(buffer) {
  let crc = 0xffffffff;
  for (const byte of buffer) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit += 1) {
      crc = crc & 1 ? (crc >>> 1) ^ 0xedb88320 : crc >>> 1;
    }
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const length = Buffer.alloc(4);
  length.writeUInt32BE(data.length);
  const body = Buffer.concat([Buffer.from(type, 'ascii'), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body));
  return Buffer.concat([length, body, crc]);
}

function png(size) {
  const pixels = render(size);
  const raw = Buffer.alloc((size * 4 + 1) * size);
  for (let y = 0; y < size; y += 1) {
    raw[y * (size * 4 + 1)] = 0; // filter: none
    pixels.copy(raw, y * (size * 4 + 1) + 1, y * size * 4, (y + 1) * size * 4);
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // colour type: RGBA
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr),
    chunk('IDAT', zlib.deflateSync(raw, { level: 9 })),
    chunk('IEND', Buffer.alloc(0)),
  ]);
}

/** ICO with PNG-compressed entries (supported since Windows Vista). */
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

const sizes = [16, 32, 48, 64, 128, 256];
const images = sizes.map((size) => ({ size, data: png(size) }));

await mkdir(outDir, { recursive: true });
await writeFile(path.join(outDir, 'icon.ico'), ico(images));
await writeFile(path.join(outDir, 'icon.png'), png(256));
console.log(`[dshell] wrote ${path.join(outDir, 'icon.ico')} (${sizes.join(', ')})`);
