'use strict';

const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const { describe, it } = require('node:test');

const { NPM_DIR, SHIM, TARGETS, tempDir } = require('./helpers');

const FAKE_HOST = path.join(__dirname, 'fake-host.js');
const LINUX = { platform: 'linux', arch: 'x64', libc: 'glibc' };
const skipOnWindows = { skip: process.platform === 'win32' && 'the fake binary is a shell script' };

// Lays out a node_modules tree holding the shim and, when `script` is given,
// a platform package whose binary is that shell script.
function install(t, target, script) {
  const dir = tempDir(t);
  const shim = path.join(dir, 'node_modules', 'kbx', 'bin', 'kbx.js');
  fs.mkdirSync(path.dirname(shim), { recursive: true });
  fs.copyFileSync(SHIM, shim);
  if (script !== undefined) {
    const pkgDir = path.join(dir, 'node_modules', target.pkg);
    fs.mkdirSync(path.join(pkgDir, 'bin'), { recursive: true });
    fs.copyFileSync(path.join(NPM_DIR, target.pkg, 'package.json'), path.join(pkgDir, 'package.json'));
    fs.writeFileSync(path.join(pkgDir, 'bin', target.file), `#!/bin/sh\n${script}\n`, { mode: 0o755 });
  }
  return shim;
}

function run(shim, { platform, arch, libc = 'glibc' }, args = [], input = undefined) {
  return spawnSync(process.execPath, ['--require', FAKE_HOST, shim, ...args], {
    encoding: 'utf8',
    input,
    env: { ...process.env, FAKE_PLATFORM: platform, FAKE_ARCH: arch, FAKE_LIBC: libc },
  });
}

function assertListsSupportedTargets(stderr) {
  for (const { pkg } of TARGETS) assert.match(stderr, new RegExp(pkg));
}

describe('supported targets', skipOnWindows, () => {
  for (const target of TARGETS) {
    it(`${target.platform}-${target.arch} runs ${target.pkg}/bin/${target.file}`, (t) => {
      const shim = install(t, target, `echo "ran ${target.pkg}"`);
      const result = run(shim, target);
      assert.equal(result.status, 0, result.stderr);
      assert.equal(result.stdout, `ran ${target.pkg}\n`);
    });
  }

  it('runs the gnu package when the libc cannot be detected', (t) => {
    const target = TARGETS.find((row) => row.pkg === 'kbx-linux-x64-gnu');
    const shim = install(t, target, 'echo ran');
    const result = run(shim, { ...LINUX, libc: 'unknown' });
    assert.equal(result.status, 0, result.stderr);
  });
});

describe('running the binary', skipOnWindows, () => {
  const linux = TARGETS.find((row) => row.pkg === 'kbx-linux-x64-gnu');

  it('passes arguments through unchanged', (t) => {
    const shim = install(t, linux, 'for arg in "$@"; do echo "[$arg]"; done');
    const result = run(shim, LINUX, ['index', 'a b', '--flag=*']);
    assert.equal(result.stdout, '[index]\n[a b]\n[--flag=*]\n');
  });

  it('inherits stdin, stdout and stderr', (t) => {
    const shim = install(t, linux, 'cat; echo oops >&2');
    const result = run(shim, LINUX, [], 'from stdin\n');
    assert.equal(result.status, 0);
    assert.equal(result.stdout, 'from stdin\n');
    assert.equal(result.stderr, 'oops\n');
  });

  for (const code of [1, 2, 7, 255]) {
    it(`exits with the binary's exit code ${code}`, (t) => {
      const shim = install(t, linux, `exit ${code}`);
      const result = run(shim, LINUX);
      assert.equal(result.status, code);
      assert.equal(result.stderr, '');
    });
  }

  for (const signal of ['SIGTERM', 'SIGKILL', 'SIGINT']) {
    it(`dies from ${signal} when the binary does`, (t) => {
      const shim = install(t, linux, `kill -${signal.slice(3)} $$`);
      const result = run(shim, LINUX);
      assert.equal(result.signal, signal);
      assert.equal(result.status, null);
    });
  }

  it('exits 128+n for a signal node ignores', (t) => {
    const shim = install(t, linux, 'kill -PIPE $$');
    const result = run(shim, LINUX);
    assert.equal(result.status, 128 + 13);
  });

  it('reports a binary that cannot be started', (t) => {
    const shim = install(t, linux, '');
    fs.chmodSync(path.join(path.dirname(shim), '..', '..', linux.pkg, 'bin', 'kbx'), 0o644);
    const result = run(shim, LINUX);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /kbx: could not run .*bin\/kbx/);
  });
});

describe('unsupported targets', () => {
  const unsupported = [
    { platform: 'freebsd', arch: 'x64', name: 'freebsd-x64' },
    { platform: 'linux', arch: 'ia32', name: 'linux-ia32' },
    { platform: 'win32', arch: 'arm64', name: 'win32-arm64' },
    { platform: 'darwin', arch: 'ppc64', name: 'darwin-ppc64' },
    { platform: 'linux', arch: 'x64', libc: 'musl', name: 'linux-x64-musl' },
    { platform: 'linux', arch: 'arm64', libc: 'musl', name: 'linux-arm64-musl' },
  ];

  for (const target of unsupported) {
    it(`${target.name} exits non-zero and lists the five targets`, (t) => {
      const result = run(install(t), target);
      assert.equal(result.status, 1);
      assert.equal(result.stdout, '');
      assert.match(result.stderr, new RegExp(`unsupported platform: ${target.name}\\b`));
      assertListsSupportedTargets(result.stderr);
    });
  }
});

describe('a supported target without its package', () => {
  it('names the missing package', (t) => {
    const result = run(install(t), LINUX);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /kbx-linux-x64-gnu is not installed/);
    assert.match(result.stderr, /optional dependencies/);
  });
});
