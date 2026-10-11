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
const manifest = path.join(root, 'src-tauri', 'Cargo.toml');

const child = spawn(cargo, ['--manifest-path', manifest, ...process.argv.slice(2)], {
  stdio: 'inherit',
  env: { ...process.env, PATH: `${cargoBin}${path.delimiter}${process.env.PATH ?? ''}` },
});
child.on('exit', (code) => process.exit(code ?? 1));
