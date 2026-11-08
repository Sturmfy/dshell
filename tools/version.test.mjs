// Tests for the version tooling. They build a throwaway tree instead of using
// the repository's own files: a bump test must never touch what it tests.

import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, mkdir, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';

import { checkVersion, versionFromTag } from './check-version.mjs';
import { setVersion } from './set-version.mjs';

async function fixture(version = '0.1.0') {
  const root = await mkdtemp(path.join(os.tmpdir(), 'dshell-version-'));
  await mkdir(path.join(root, 'src-tauri'), { recursive: true });
  await writeFile(
    path.join(root, 'package.json'),
    `${JSON.stringify({ name: 'dshell', private: true, version }, null, 2)}\n`,
  );
  await writeFile(
    path.join(root, 'src-tauri/tauri.conf.json'),
    `${JSON.stringify({ productName: 'dshell', version }, null, 2)}\n`,
  );
  await writeFile(
    path.join(root, 'src-tauri/Cargo.toml'),
    `[package]\nname = "dshell"\nversion = "${version}"\nedition = "2021"\n\n[dependencies]\n`,
  );
  await writeFile(
    path.join(root, 'src-tauri/Cargo.lock'),
    '[[package]]\nname = "dshell"\nversion = "' +
      version +
      '"\ndependencies = []\n\n[[package]]\nname = "serde"\nversion = "1.0.0"\n',
  );
  return root;
}

test('setVersion updates every file', async () => {
  const root = await fixture();
  try {
    const { changed } = await setVersion({ root, version: '0.2.0' });
    assert.deepEqual(changed.sort(), [
      'package.json',
      'src-tauri/Cargo.lock',
      'src-tauri/Cargo.toml',
      'src-tauri/tauri.conf.json',
    ]);
    const check = await checkVersion({ root, tag: 'v0.2.0' });
    assert.deepEqual(check.problems, []);
    const lock = await readFile(path.join(root, 'src-tauri/Cargo.lock'), 'utf8');
    assert.match(lock, /name = "dshell"\nversion = "0\.2\.0"/);
    // The unrelated package keeps its own version.
    assert.match(lock, /name = "serde"\nversion = "1\.0\.0"/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('a second identical bump changes nothing', async () => {
  const root = await fixture('0.2.0');
  try {
    const { changed } = await setVersion({ root, version: '0.2.0' });
    assert.deepEqual(changed, []);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('setVersion refuses a non-SemVer version', async () => {
  const root = await fixture();
  try {
    await assert.rejects(() => setVersion({ root, version: '0.2' }), /not a SemVer/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('checkVersion catches one file drifting', async () => {
  const root = await fixture();
  try {
    await writeFile(
      path.join(root, 'src-tauri/tauri.conf.json'),
      `${JSON.stringify({ version: '0.1.1' }, null, 2)}\n`,
    );
    const check = await checkVersion({ root });
    assert.equal(check.problems.length, 1);
    assert.match(check.problems[0], /disagree/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('checkVersion rejects a tag that names another version', async () => {
  const root = await fixture('0.1.0');
  try {
    const check = await checkVersion({ root, tag: 'v0.2.0' });
    assert.equal(check.problems.length, 1);
    assert.match(check.problems[0], /bump first, then tag/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('prerelease tags are valid and must match exactly', async () => {
  const root = await fixture('0.2.0-beta.1');
  try {
    assert.deepEqual((await checkVersion({ root, tag: 'v0.2.0-beta.1' })).problems, []);
    assert.equal((await checkVersion({ root, tag: 'v0.2.0' })).problems.length, 1);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('versionFromTag enforces the v prefix and SemVer', () => {
  assert.equal(versionFromTag('v1.2.3'), '1.2.3');
  assert.equal(versionFromTag('v0.2.0-rc.1'), '0.2.0-rc.1');
  assert.throws(() => versionFromTag('1.2.3'), /must start with "v"/);
  assert.throws(() => versionFromTag('v1.2'), /does not name a SemVer/);
  assert.throws(() => versionFromTag('v1.2.3+build'), /does not name a SemVer/);
});
