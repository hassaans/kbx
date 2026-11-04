#!/usr/bin/env node
'use strict';

const { execFileSync } = require('node:child_process');
const os = require('node:os');

const PACKAGES = {
  'darwin-arm64': 'kbx-darwin-arm64',
  'darwin-x64': 'kbx-darwin-x64',
  'linux-x64': 'kbx-linux-x64-gnu',
  'linux-arm64': 'kbx-linux-arm64-gnu',
  'win32-x64': 'kbx-win32-x64-msvc',
};

function fail(message) {
  console.error(`kbx: ${message}`);
  process.exit(1);
}

// An undetectable libc falls through to the gnu package: most such hosts are
// glibc, and a wrong guess fails at exec with its own error.
function isMusl() {
  if (process.platform !== 'linux') return false;
  const { MUSL, familySync } = require('detect-libc');
  return familySync() === MUSL;
}

function packageName() {
  const target = `${process.platform}-${process.arch}`;
  const musl = isMusl();
  if (!musl && PACKAGES[target]) return PACKAGES[target];
  const supported = Object.entries(PACKAGES)
    .map(([name, pkg]) => `  ${name} (${pkg})`)
    .join('\n');
  fail(
    `unsupported platform: ${target}${musl ? '-musl' : ''}\n` +
      `Supported targets:\n${supported}\n` +
      'On anything else, build from source with `cargo install kbx`.',
  );
}

function resolveBinary(pkg) {
  const file = process.platform === 'win32' ? 'kbx.exe' : 'kbx';
  try {
    return require.resolve(`${pkg}/bin/${file}`);
  } catch {
    fail(
      `${pkg} is not installed. It comes with kbx as one of its optional dependencies, ` +
        'so reinstall kbx without --omit=optional or --no-optional.',
    );
  }
}

const binary = resolveBinary(packageName());

try {
  execFileSync(binary, process.argv.slice(2), { stdio: 'inherit' });
} catch (err) {
  if (err.signal) {
    process.kill(process.pid, err.signal);
    // Only reached when this process ignores the signal, as node does SIGPIPE.
    process.exit(128 + os.constants.signals[err.signal]);
  }
  if (typeof err.status === 'number') process.exit(err.status);
  fail(`could not run ${binary}: ${err.message}`);
}
