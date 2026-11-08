// Read-only version gate: every file must agree, and a release tag must name
// the version those files carry.
//
//   node tools/check-version.mjs          # files only
//   node tools/check-version.mjs v0.2.0   # files + tag (what CI runs)
//
// Exits non-zero with one line per problem. The point is to fail before a build
// burns ten minutes, not after a release ships with the wrong stamp.

import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import { VERSION_FILES, VERSION_RE } from './set-version.mjs';

/** `vX.Y.Z` or `vX.Y.Z-pre.N` → the version inside the tag. */
export function versionFromTag(tag) {
  if (!tag.startsWith('v')) {
    throw new Error(`release tag "${tag}" must start with "v"`);
  }
  const version = tag.slice(1);
  if (!VERSION_RE.test(version)) {
    throw new Error(`release tag "${tag}" does not name a SemVer version`);
  }
  return version;
}

function sectionVersion(text, file) {
  if (file === 'src-tauri/Cargo.toml') {
    let inPackage = false;
    for (const line of text.split(/\r?\n/)) {
      const trimmed = line.trim();
      if (/^\[.+\]$/.test(trimmed)) {
        inPackage = trimmed === '[package]';
        continue;
      }
      if (inPackage) {
        const match = /^version\s*=\s*"([^"]+)"/.exec(trimmed);
        if (match) {
          return match[1];
        }
      }
    }
    throw new Error(`${file} has no [package] version`);
  }
  if (file === 'src-tauri/Cargo.lock') {
    const match =
      /\[\[package\]\]\r?\nname = "dshell"\r?\nversion = "([^"]+)"/.exec(text);
    if (!match) {
      throw new Error(`${file} has no \`dshell\` package entry`);
    }
    return match[1];
  }
  const parsed = JSON.parse(text);
  if (typeof parsed.version !== 'string') {
    throw new Error(`${file} has no string \`version\``);
  }
  return parsed.version;
}

/**
 * Check the tree under `root`.
 *
 * Returns `{ version, tag, problems }` instead of throwing, so the CLI and the
 * tests share one definition of "consistent".
 */
export async function checkVersion({ root, tag }) {
  const problems = [];
  const found = [];

  for (const relative of VERSION_FILES) {
    const file = path.join(root, relative);
    try {
      const text = await readFile(file, 'utf8');
      const version = sectionVersion(text, relative);
      found.push({ file: relative, version });
      if (!VERSION_RE.test(version)) {
        problems.push(`${relative}: "${version}" is not a SemVer version`);
      }
    } catch (error) {
      problems.push(`${relative}: ${error.message}`);
    }
  }

  const versions = new Set(found.map((entry) => entry.version));
  if (versions.size > 1) {
    const detail = found.map((entry) => `  ${entry.file} = ${entry.version}`).join('\n');
    problems.push(`the version files disagree:\n${detail}`);
  }

  const version = found[0]?.version ?? null;
  if (version === '0.0.0') {
    problems.push('version 0.0.0 is a placeholder, not a release');
  }

  let tagVersion = null;
  if (tag !== undefined && tag !== null && tag !== '') {
    try {
      tagVersion = versionFromTag(tag);
    } catch (error) {
      problems.push(error.message);
    }
    if (tagVersion && version && tagVersion !== version) {
      problems.push(
        `tag ${tag} says ${tagVersion}, but package.json says ${version} — ` +
          'bump first, then tag the commit that carries the bump',
      );
    }
  }

  return { version, tag: tagVersion, problems };
}

async function main(argv) {
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
  // Deliberately no `GITHUB_REF_NAME` fallback: on a branch push that variable
  // is `main`, which is not a tag. The release workflow passes the tag
  // explicitly.
  const tag = argv[2];

  const { version, tag: tagVersion, problems } = await checkVersion({ root, tag });
  if (problems.length > 0) {
    console.error('[dshell] version check failed:');
    for (const problem of problems) {
      console.error(`  - ${problem}`);
    }
    process.exit(1);
  }

  const suffix = tagVersion ? `, tag ${argv[2] ?? process.env.GITHUB_REF_NAME}` : '';
  console.log(`[dshell] version ${version} is consistent across ${VERSION_FILES.length} files${suffix}`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await main(process.argv);
}
