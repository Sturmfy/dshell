// Stages the deliverable: one `dshell.exe` in `release/`.
//
// The whole point of this project is that the artifact is a single file. No DLL
// is shipped next to it — the frontend is embedded in the executable and
// WebView2Loader is statically imported, so copying the exe anywhere is the
// entire install procedure. This script verifies that claim instead of asserting
// it: after staging, it checks the exe's directory contains nothing but the exe.

import { copyFile, mkdir, readdir, rm, stat } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const exeName = process.platform === 'win32' ? 'dshell.exe' : 'dshell';
const built = path.join(root, 'src-tauri', 'target', 'release', exeName);
const outDir = path.join(root, 'release');
const staged = path.join(outDir, exeName);

async function main() {
  try {
    await stat(built);
  } catch {
    console.error(`[dshell] no release build found at ${built}\n         run: npm run build`);
    process.exit(1);
  }

  await rm(outDir, { recursive: true, force: true });
  await mkdir(outDir, { recursive: true });
  await copyFile(built, staged);

  const entries = await readdir(outDir);
  const size = (await stat(staged)).size;
  console.log(`[dshell] ${staged}  (${(size / 1024 / 1024).toFixed(2)} MB)`);
  console.log(`[dshell] contents of release/: ${entries.join(', ')}`);

  const extra = entries.filter((name) => name !== exeName);
  if (extra.length > 0) {
    console.warn(
      `[dshell] WARNING: release/ contains more than the executable (${extra.join(', ')}).\n` +
        '         The single-binary claim no longer holds — investigate before shipping.',
    );
    process.exit(1);
  }
  console.log('[dshell] single binary confirmed: copy it anywhere and run it.');
}

await main();
