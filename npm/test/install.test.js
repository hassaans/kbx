'use strict';

const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const { describe, it } = require('node:test');

const { TARGETS, pack, tempDir } = require('./helpers');

const WINDOWS = process.platform === 'win32';
const BINARY = process.env.KBX_BINARY && path.resolve(process.env.KBX_BINARY);
const HOST = TARGETS.find(
  (row) => row.platform === process.platform && row.arch === process.arch,
);
const { version } = require('../kbx/package.json');

// .cmd files can only be started through a shell on Windows.
function exec(command, args) {
  return spawnSync(command, args, { encoding: 'utf8', shell: WINDOWS });
}

describe('global install from local tarballs', () => {
  it(
    'gives a working kbx',
    { skip: !BINARY && 'set KBX_BINARY to a built kbx', timeout: 300_000 },
    (t) => {
      const dir = tempDir(t);
      const prefix = path.join(dir, 'prefix');
      const shim = pack('kbx', dir);
      const platform = pack(HOST.pkg, dir, BINARY);

      const installed = exec('npm', ['i', '-g', '--ignore-scripts', '--prefix', prefix, shim, platform]);
      assert.equal(installed.status, 0, installed.stderr);

      const kbx = WINDOWS ? path.join(prefix, 'kbx.cmd') : path.join(prefix, 'bin', 'kbx');
      const modules = WINDOWS
        ? path.join(prefix, 'node_modules')
        : path.join(prefix, 'lib', 'node_modules');
      assert.ok(fs.existsSync(path.join(modules, HOST.pkg, 'package.json')));
      for (const other of TARGETS.filter((row) => row !== HOST)) {
        assert.equal(fs.existsSync(path.join(modules, other.pkg)), false);
        assert.equal(fs.existsSync(path.join(modules, 'kbx', 'node_modules', other.pkg)), false);
      }

      const result = exec(kbx, ['--version']);
      assert.equal(result.status, 0, result.stderr);
      assert.equal(result.stdout.trim(), `kbx ${version}`);

      // clap exits 2 on a usage error; the shim has to pass that through.
      assert.equal(exec(kbx, ['--no-such-flag']).status, 2);
    },
  );
});
