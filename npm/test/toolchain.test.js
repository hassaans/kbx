'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { describe, it } = require('node:test');

const { REPO } = require('./helpers');

const WORKFLOWS = path.join(REPO, '.github', 'workflows');

function read(...parts) {
  return fs.readFileSync(path.join(REPO, ...parts), 'utf8');
}

// The steps of a workflow, one string each, so a test can ask what a step does
// without parsing YAML.
function steps(workflow) {
  return workflow.split(/^ {6}- (?=\w)/m).slice(1);
}

// rust-toolchain.toml is the one place the Rust version is written. CI and the
// release build follow it, so a new stable can't change what they build with.
describe('the pinned Rust toolchain', () => {
  const toolchain = read('rust-toolchain.toml');

  it('names one exact release, with clippy and rustfmt', () => {
    assert.match(toolchain, /^\[toolchain\]\r?$/m);
    assert.match(toolchain, /^channel = "\d+\.\d+\.\d+"\r?$/m);
    assert.match(toolchain, /^components = \[(?=.*"clippy")(?=.*"rustfmt").*\]\r?$/m);
  });

  it('leaves out targets, so a local build downloads only its own', () => {
    assert.doesNotMatch(toolchain, /^targets\s*=/m);
  });

  for (const file of ['ci.yml', 'release.yml']) {
    describe(file, () => {
      const workflow = read('.github', 'workflows', file);

      it('names no Rust version of its own', () => {
        assert.doesNotMatch(workflow, /dtolnay\/rust-toolchain/);
        assert.doesNotMatch(workflow, /RUSTUP_TOOLCHAIN/);
        assert.doesNotMatch(workflow, /\bcargo \+/);
        assert.doesNotMatch(workflow, /rustup (?:default|override)/);
      });

      it('installs the pinned toolchain before every cargo step', () => {
        const jobs = workflow.slice(workflow.indexOf('\njobs:')).split(/^ {2}(?=[\w-]+:\r?$)/m).slice(1);
        const cargoJobs = jobs.filter((job) => /^ +(?:- )?run: .*\bcargo /m.test(job) || /^ +cargo /m.test(job) || /publish-crate\.sh/.test(job));
        assert.ok(cargoJobs.length > 0);
        for (const job of cargoJobs) {
          const name = job.slice(0, job.indexOf(':'));
          const all = steps(job);
          const install = all.findIndex((step) => /^run: rustup toolchain install\r?$/m.test(step));
          const cargo = all.findIndex((step) => /\bcargo |publish-crate\.sh/.test(step));
          assert.ok(install >= 0, `${name} never installs the pinned toolchain`);
          assert.ok(install < cargo, `${name} runs cargo before installing the toolchain`);
        }
      });
    });
  }

  it('adds each release target to the pinned toolchain, not to stable', () => {
    const workflow = read('.github', 'workflows', 'release.yml');
    const build = workflow.slice(workflow.indexOf('\n  build:'), workflow.indexOf('\n  pack:'));
    const all = steps(build);
    const install = all.findIndex((step) => /^run: rustup toolchain install\r?$/m.test(step));
    const target = all.findIndex((step) => /^run: rustup target add \$\{\{ matrix\.target \}\}\r?$/m.test(step));
    const cargo = all.findIndex((step) => /\bcargo build\b/.test(step));
    assert.ok(install >= 0 && install < target && target < cargo, 'expected install, then target add, then cargo build');
  });
});

// A user's `cargo install kbx` builds with whatever Rust they have, so the
// smoke check does the same: on stable, outside the checkout and its pin.
describe('smoke.yml', () => {
  const workflow = read('.github', 'workflows', 'smoke.yml');

  it('installs kbx from crates.io on stable, away from rust-toolchain.toml', () => {
    const install = steps(workflow).find((step) => /cargo install kbx/.test(step));
    assert.ok(install, 'no cargo install step');
    assert.match(install, /^ {8}working-directory: \$\{\{ runner\.temp \}\}\r?$/m);
    assert.match(workflow, /RUSTUP_TOOLCHAIN: stable|toolchain install stable|dtolnay\/rust-toolchain@stable/);
  });
});

// The early warning: what the next stable says about the code, weekly, without
// ever standing in the way of a merge.
describe('stable.yml', () => {
  const workflow = read('.github', 'workflows', 'stable.yml');
  const triggers = workflow.slice(workflow.indexOf('\non:'), workflow.indexOf('\npermissions:'));

  it('runs weekly and by hand, and never on a push or a pull request', () => {
    assert.match(triggers, /^ {2}schedule:\r?\n {4}- cron: '(?:\S+ ){4}\d'\r?$/m);
    assert.match(triggers, /^ {2}workflow_dispatch:/m);
    assert.doesNotMatch(triggers, /^ {2}(?:push|pull_request|pull_request_target|merge_group):/m);
  });

  it('builds with stable, over the pin', () => {
    assert.match(workflow, /^ {6}RUSTUP_TOOLCHAIN: stable\r?$/m);
    assert.match(workflow, /rustup toolchain install stable --profile minimal --component rustfmt,clippy/);
  });

  it('runs fmt, clippy and the tests as CI does', () => {
    const ci = read('.github', 'workflows', 'ci.yml');
    for (const command of [
      'cargo fmt --all --check',
      'cargo clippy --workspace --all-targets --locked -- -D warnings',
      'cargo test --workspace --locked',
    ]) {
      assert.ok(ci.includes(`run: ${command}\n`) || ci.includes(`run: ${command}\r\n`), `ci.yml lost ${command}`);
      assert.ok(workflow.includes(`run: ${command}\n`) || workflow.includes(`run: ${command}\r\n`), `stable.yml lacks ${command}`);
    }
  });

  it('reports every failure, rather than stopping at the first', () => {
    const job = workflow.slice(workflow.indexOf('\njobs:'));
    assert.match(job, /fail-fast: false/);
    for (const step of steps(job).filter((step) => /run: cargo (?:clippy|test)/.test(step))) {
      assert.match(step, /^ {8}if: \$\{\{ !cancelled\(\) \}\}\r?$/m);
    }
  });

  it('is the only workflow on stable that runs the lints', () => {
    const others = fs.readdirSync(WORKFLOWS).filter((file) => file !== 'stable.yml');
    for (const file of others) {
      const text = read('.github', 'workflows', file);
      assert.ok(!(/stable/.test(text) && /cargo clippy/.test(text)), `${file} lints on stable`);
    }
  });
});

describe('docs/releasing.md', () => {
  it('says how to bump the pinned Rust version', () => {
    const doc = read('docs', 'releasing.md');
    assert.match(doc, /^## Bump the Rust version\r?$/m);
    assert.match(doc, /rust-toolchain\.toml/);
    assert.match(doc, /cargo clippy --workspace --all-targets --locked -- -D warnings/);
  });
});
