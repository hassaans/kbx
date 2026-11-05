'use strict';

const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const zlib = require('node:zlib');

const REPO = path.resolve(__dirname, '..', '..');
const NPM_DIR = path.join(REPO, 'npm');
const SHIM = path.join(NPM_DIR, 'kbx-cli', 'bin', 'kbx.js');

// One row per release target: what the shim sees, and what it should run.
const TARGETS = [
  { platform: 'darwin', arch: 'arm64', pkg: 'kbx-darwin-arm64', file: 'kbx' },
  { platform: 'darwin', arch: 'x64', pkg: 'kbx-darwin-x64', file: 'kbx' },
  { platform: 'linux', arch: 'x64', pkg: 'kbx-linux-x64-gnu', file: 'kbx' },
  { platform: 'linux', arch: 'arm64', pkg: 'kbx-linux-arm64-gnu', file: 'kbx' },
  { platform: 'win32', arch: 'x64', pkg: 'kbx-windows-x64', file: 'kbx.exe' },
];

function tempDir(t) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'kbx-npm-')));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
}

// Runs npm/pack.sh and returns the tarball path it prints.
function pack(pkg, outDir, binary) {
  const args = [path.join(NPM_DIR, 'pack.sh'), pkg, outDir];
  if (binary) args.push(binary);
  const result = spawnSync('bash', args, { encoding: 'utf8' });
  if (result.status !== 0) {
    throw new Error(`pack.sh ${pkg} failed:\n${result.stdout}${result.stderr}`);
  }
  return result.stdout.trim().split('\n').pop();
}

// Lists a .tgz as [{ name, mode }], with npm's leading `package/` removed.
function readTarball(tarball) {
  const tar = zlib.gunzipSync(fs.readFileSync(tarball));
  const entries = [];
  for (let offset = 0; offset + 512 <= tar.length && tar[offset] !== 0; ) {
    const field = (start, length) =>
      tar.toString('latin1', offset + start, offset + start + length).replace(/\0.*$/s, '');
    const size = parseInt(field(124, 12), 8);
    entries.push({
      name: field(0, 100).replace(/^package\//, ''),
      mode: parseInt(field(100, 8), 8),
    });
    offset += 512 + Math.ceil(size / 512) * 512;
  }
  return entries;
}

module.exports = { NPM_DIR, REPO, SHIM, TARGETS, pack, readTarball, tempDir };
