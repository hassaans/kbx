'use strict';

const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const { describe, it } = require('node:test');

const { NPM_DIR, REPO, TARGETS, pack, readTarball, tempDir } = require('./helpers');

function names(tarball) {
  return readTarball(tarball).map((entry) => entry.name).sort();
}

function fakeBinary(dir) {
  const binary = path.join(dir, 'fake-binary');
  fs.writeFileSync(binary, 'not really a binary\n');
  return binary;
}

describe('npm pack', () => {
  it('packs the shim with only its entry point, README and licence', (t) => {
    const tarball = pack('kbx-cli', tempDir(t));
    assert.equal(path.basename(tarball), `kbx-cli-${require('../kbx-cli/package.json').version}.tgz`);
    assert.deepEqual(names(tarball), ['LICENSE', 'README.md', 'bin/kbx.js', 'package.json']);
  });

  for (const { pkg, file } of TARGETS) {
    it(`packs ${pkg} with only package.json and the binary`, (t) => {
      const dir = tempDir(t);
      const tarball = pack(pkg, dir, fakeBinary(dir));
      assert.deepEqual(names(tarball), [`bin/${file}`, 'package.json'].sort());
    });
  }

  it('keeps the binary executable', { skip: process.platform === 'win32' && 'no exec bit' }, (t) => {
    const dir = tempDir(t);
    const entries = readTarball(pack('kbx-linux-x64-gnu', dir, fakeBinary(dir)));
    const binary = entries.find((entry) => entry.name === 'bin/kbx');
    assert.equal(binary.mode & 0o111, 0o111);
  });

  it('leaves the checkout untouched', (t) => {
    const dir = tempDir(t);
    pack('kbx-darwin-arm64', dir, fakeBinary(dir));
    assert.equal(fs.existsSync(path.join(NPM_DIR, 'kbx-darwin-arm64', 'bin')), false);
  });

  it('refuses a platform package without a binary', (t) => {
    const result = spawnSync('bash', [path.join(NPM_DIR, 'pack.sh'), 'kbx-darwin-arm64', tempDir(t)], {
      encoding: 'utf8',
    });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /needs a binary/);
  });

  it('refuses an unknown package', (t) => {
    const result = spawnSync('bash', [path.join(NPM_DIR, 'pack.sh'), 'kbx-plan9', tempDir(t)], {
      encoding: 'utf8',
    });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /unknown package/);
  });
});

describe('package manifests', () => {
  const manifests = ['kbx-cli', ...TARGETS.map((row) => row.pkg)].map((name) => [
    name,
    JSON.parse(fs.readFileSync(path.join(NPM_DIR, name, 'package.json'), 'utf8')),
  ]);

  for (const [name, manifest] of manifests) {
    it(`${name} has no lifecycle scripts`, () => {
      assert.equal(manifest.name, name);
      assert.equal(manifest.scripts, undefined);
    });
  }

  for (const { platform, arch, pkg } of TARGETS) {
    it(`${pkg} installs only on ${platform}-${arch}`, () => {
      const manifest = new Map(manifests).get(pkg);
      assert.deepEqual(manifest.os, [platform]);
      assert.deepEqual(manifest.cpu, [arch]);
      assert.deepEqual(manifest.libc, platform === 'linux' ? ['glibc'] : undefined);
      assert.equal(manifest.bin, undefined);
    });
  }

  it('installs the shim as the kbx command', () => {
    const manifest = new Map(manifests).get('kbx-cli');
    assert.deepEqual(manifest.bin, { kbx: 'bin/kbx.js' });
    assert.equal(manifest.repository.directory, 'npm/kbx-cli');
  });

  it('pins the shim dependency exactly', () => {
    const { dependencies } = new Map(manifests).get('kbx-cli');
    assert.deepEqual(Object.keys(dependencies), ['detect-libc']);
    assert.match(dependencies['detect-libc'], /^\d+\.\d+\.\d+$/);
  });

  it('ships the repository licence with the shim', () => {
    assert.deepEqual(
      fs.readFileSync(path.join(NPM_DIR, 'kbx-cli', 'LICENSE')),
      fs.readFileSync(path.join(REPO, 'LICENSE')),
    );
  });
});
