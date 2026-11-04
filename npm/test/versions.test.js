'use strict';

const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const { describe, it } = require('node:test');

const { NPM_DIR, REPO, TARGETS, tempDir } = require('./helpers');

const CHECK = path.join(NPM_DIR, 'check-versions.js');
const PACKAGES = ['kbx', ...TARGETS.map((row) => row.pkg)];

function check(root) {
  return spawnSync(process.execPath, root ? [CHECK, root] : [CHECK], { encoding: 'utf8' });
}

// Copies the files the check reads into a throwaway repository root.
function copyRepo(t) {
  const root = tempDir(t);
  fs.copyFileSync(path.join(REPO, 'Cargo.toml'), path.join(root, 'Cargo.toml'));
  for (const name of PACKAGES) {
    fs.mkdirSync(path.join(root, 'npm', name), { recursive: true });
    fs.copyFileSync(path.join(NPM_DIR, name, 'package.json'), path.join(root, 'npm', name, 'package.json'));
  }
  return root;
}

function edit(root, name, change) {
  const file = path.join(root, 'npm', name, 'package.json');
  const manifest = JSON.parse(fs.readFileSync(file, 'utf8'));
  change(manifest);
  fs.writeFileSync(file, JSON.stringify(manifest));
}

describe('check-versions.js', () => {
  it('passes on the repository', () => {
    const result = check();
    assert.equal(result.status, 0, result.stderr);
  });

  it('passes on an unchanged copy', (t) => {
    assert.equal(check(copyRepo(t)).status, 0);
  });

  it('fails when the Cargo workspace moves on', (t) => {
    const root = copyRepo(t);
    const file = path.join(root, 'Cargo.toml');
    const cargo = fs.readFileSync(file, 'utf8');
    fs.writeFileSync(file, cargo.replace(/^version = ".*"$/m, 'version = "9.9.9"'));
    const result = check(root);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /npm\/kbx\/package.json/);
    assert.match(result.stderr, /9\.9\.9/);
  });

  for (const name of PACKAGES) {
    it(`fails when ${name} drifts`, (t) => {
      const root = copyRepo(t);
      edit(root, name, (manifest) => {
        manifest.version = '9.9.9';
      });
      const result = check(root);
      assert.equal(result.status, 1);
      assert.match(result.stderr, new RegExp(`npm/${name}/package.json`));
    });
  }

  it('fails when an optional dependency pin drifts', (t) => {
    const root = copyRepo(t);
    edit(root, 'kbx', (manifest) => {
      manifest.optionalDependencies['kbx-darwin-x64'] = '9.9.9';
    });
    const result = check(root);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /kbx-darwin-x64/);
  });

  it('fails when an optional dependency is missing', (t) => {
    const root = copyRepo(t);
    edit(root, 'kbx', (manifest) => {
      delete manifest.optionalDependencies['kbx-win32-x64-msvc'];
    });
    const result = check(root);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /kbx-win32-x64-msvc/);
  });
});
