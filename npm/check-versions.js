#!/usr/bin/env node
'use strict';

// Fails when an npm package, or a pin in the shim, differs from the Cargo
// workspace version. Takes the repository root, defaulting to this checkout.
const fs = require('node:fs');
const path = require('node:path');

const PLATFORM_PACKAGES = [
  'kbx-darwin-arm64',
  'kbx-darwin-x64',
  'kbx-linux-arm64-gnu',
  'kbx-linux-x64-gnu',
  'kbx-windows-x64',
];

const root = path.resolve(process.argv[2] || path.join(__dirname, '..'));

const cargo = fs.readFileSync(path.join(root, 'Cargo.toml'), 'utf8');
const section = cargo.split(/^\[workspace\.package\]$/m)[1] || '';
const match = section.match(/^version = "(.+)"$/m);
if (!match) {
  console.error('check-versions: no version in [workspace.package] of Cargo.toml');
  process.exit(1);
}
const version = match[1];

function manifest(name) {
  return JSON.parse(fs.readFileSync(path.join(root, 'npm', name, 'package.json'), 'utf8'));
}

const errors = [];
for (const name of ['kbx', ...PLATFORM_PACKAGES]) {
  const actual = manifest(name).version;
  if (actual !== version) errors.push(`npm/${name}/package.json is at ${actual}`);
}

const pins = manifest('kbx').optionalDependencies || {};
for (const name of PLATFORM_PACKAGES) {
  if (pins[name] !== version) {
    errors.push(`npm/kbx/package.json pins ${name} to ${pins[name]}`);
  }
}

if (errors.length > 0) {
  console.error(`check-versions: Cargo.toml is at ${version}, but:`);
  for (const error of errors) console.error(`  ${error}`);
  process.exit(1);
}
