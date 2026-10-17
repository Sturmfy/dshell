// Runs cargo with the user-level toolchain on PATH, since rustup's installer does
// not update PATH for shells that are already running.
import { spawn } from 'node:child_process';
import path from 'node:path';
import os from 'node:os';
import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const cargoBin = path.join(os.homedir(), '.cargo', 'bin');
const exe = process.platform === 'win32' ? 'cargo.exe' : 'cargo';
const cargo = existsSync(path.join(cargoBin, exe)) ? path.join(cargoBin, exe) : exe;

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const crateDir = path.join(root, 'src-tauri');

// Run from the crate directory rather than passing `--manifest-path`.
//
// cargo 1.99 rejects `cargo --manifest-path <path> <command>` ("unexpected
// argument"), and appending the flag instead breaks the `-- <rustc args>` form
// that `npm run check` uses — everything after `--` is a rustc argument, so a
// cargo flag may not be placed there. The manifest is simply the working
// directory, which no cargo version has an opinion about.
const child = spawn(cargo, process.argv.slice(2), {
  cwd: crateDir,
  stdio: 'inherit',
  env: { ...process.env, PATH: `${cargoBin}${path.delimiter}${process.env.PATH ?? ''}` },
});
child.on('exit', (code) => process.exit(code ?? 1));
