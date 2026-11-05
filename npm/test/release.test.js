'use strict';

const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const { describe, it } = require('node:test');

const { NPM_DIR, REPO, TARGETS, tempDir } = require('./helpers');

const SCRIPTS = path.join(REPO, '.github', 'scripts');
const VERSION = require('../kbx/package.json').version;
const TAG = `v${VERSION}`;

// Every tool that can publish. A test that doesn't fake one gets a stub that
// fails, so these tests can never reach a real registry.
const PUBLISHERS = { cargo: 'exit 97', curl: 'exit 97', gh: 'exit 97', npm: 'exit 97' };

// Puts fake tools on PATH, given as { name: bash body }, and runs a release
// script with them. Returns the result plus each fake's calls, one per line.
function run(t, script, args, { tools = {}, env = {}, scripts = SCRIPTS, cwd } = {}) {
  const dir = tempDir(t);
  const calls = path.join(dir, 'calls');
  const output = path.join(dir, 'output');
  fs.writeFileSync(calls, '');
  fs.writeFileSync(output, '');
  for (const [name, body] of Object.entries({ ...PUBLISHERS, ...tools })) {
    const file = path.join(dir, name);
    fs.writeFileSync(file, `#!/usr/bin/env bash\necho "${name} $*" >> "$CALLS"\n${body}\n`);
    fs.chmodSync(file, 0o755);
  }
  // BASH_ENV would run the user's shell startup, which can put the real tools
  // back ahead of the fakes on PATH. The placeholder credentials are the second
  // line of defence if a real tool is ever reached.
  const { BASH_ENV, ...inherited } = process.env;
  const result = spawnSync('bash', [path.join(scripts, script), ...args], {
    cwd,
    encoding: 'utf8',
    env: {
      ...inherited,
      PATH: `${dir}${path.delimiter}${process.env.PATH}`,
      CALLS: calls,
      GITHUB_OUTPUT: output,
      GH_TOKEN: 'placeholder',
      CARGO_REGISTRY_TOKEN: 'placeholder',
      NODE_AUTH_TOKEN: 'placeholder',
      KBX_CRATE_POLL_SECONDS: '0',
      ...env,
    },
  });
  return {
    ...result,
    calls: fs.readFileSync(calls, 'utf8').split('\n').filter(Boolean),
    output: fs.readFileSync(output, 'utf8'),
  };
}

function preflight(t, env, options) {
  const base = { GITHUB_REF: 'refs/heads/main', DRY_RUN: '', NPM_TOKEN: '', CARGO_REGISTRY_TOKEN: '' };
  return run(t, 'preflight.sh', [], { env: { ...base, ...env }, ...options });
}

const TOKENS = { NPM_TOKEN: 'npm-secret', CARGO_REGISTRY_TOKEN: 'cargo-secret' };
const NPM_ACCEPTS_TOKEN = { tools: { npm: 'exit 0' } };

describe('release scripts', { skip: process.platform === 'win32' && 'they run on Linux' }, () => {
  it('runs the fakes, not the real tools', (t) => {
    const dir = tempDir(t);
    const probe = path.join(dir, 'probe.sh');
    fs.writeFileSync(probe, 'for tool in cargo curl gh npm; do "$tool" --version || echo "$tool stubbed"; done\n');
    const result = run(t, 'probe.sh', [], { scripts: dir });
    for (const tool of ['cargo', 'curl', 'gh', 'npm']) {
      assert.match(result.stdout, new RegExp(`^${tool} stubbed$`, 'm'));
    }
    assert.equal(result.calls.length, 4);
  });

  describe('preflight.sh', () => {
    it('does not publish from a branch, and needs no tokens there', (t) => {
      const result = preflight(t, {});
      assert.equal(result.status, 0, result.stderr);
      assert.match(result.output, /^publish=false$/m);
      assert.match(result.output, new RegExp(`^version=${VERSION}$`, 'm'));
      assert.deepEqual(result.calls, []);
    });

    it('does not publish from a pull request', (t) => {
      const result = preflight(t, { GITHUB_REF: 'refs/pull/7/merge', ...TOKENS });
      assert.equal(result.status, 0, result.stderr);
      assert.match(result.output, /^publish=false$/m);
    });

    it('publishes from a matching tag when both tokens are set', (t) => {
      const result = preflight(t, { GITHUB_REF: `refs/tags/${TAG}`, ...TOKENS }, NPM_ACCEPTS_TOKEN);
      assert.equal(result.status, 0, result.stderr);
      assert.match(result.output, /^publish=true$/m);
    });

    it('checks NPM_TOKEN against the registry before publishing', (t) => {
      const npm = '[[ $NODE_AUTH_TOKEN == npm-secret ]]';
      const result = preflight(t, { GITHUB_REF: `refs/tags/${TAG}`, ...TOKENS }, { tools: { npm } });
      assert.equal(result.status, 0, result.stderr);
      assert.deepEqual(result.calls, ['npm whoami']);
    });

    it('fails when the registry refuses NPM_TOKEN', (t) => {
      const npm = 'echo "npm error code E401" >&2; exit 1';
      const result = preflight(t, { GITHUB_REF: `refs/tags/${TAG}`, ...TOKENS }, { tools: { npm } });
      assert.equal(result.status, 1);
      assert.match(result.stderr, /E401/);
      assert.match(result.stderr, /NPM_TOKEN/);
      assert.doesNotMatch(result.output, /^publish=/m);
    });

    it('does not publish from a tag on a dry run, and needs no tokens there', (t) => {
      const result = preflight(t, { GITHUB_REF: `refs/tags/${TAG}`, DRY_RUN: 'true' });
      assert.equal(result.status, 0, result.stderr);
      assert.match(result.output, /^publish=false$/m);
    });

    for (const [missing, present] of [
      ['NPM_TOKEN', 'CARGO_REGISTRY_TOKEN'],
      ['CARGO_REGISTRY_TOKEN', 'NPM_TOKEN'],
    ]) {
      it(`fails without ${missing}, naming only that secret`, (t) => {
        const result = preflight(t, { GITHUB_REF: `refs/tags/${TAG}`, [present]: 'secret' });
        assert.equal(result.status, 1);
        assert.match(result.stdout + result.stderr, new RegExp(`${missing} is not set`));
        assert.doesNotMatch(result.stdout + result.stderr, new RegExp(`${present} is not set`));
        assert.doesNotMatch(result.output, /^publish=/m);
      });
    }

    it('names both secrets when both are missing', (t) => {
      const result = preflight(t, { GITHUB_REF: `refs/tags/${TAG}` });
      assert.equal(result.status, 1);
      assert.match(result.stderr, /NPM_TOKEN is not set/);
      assert.match(result.stderr, /CARGO_REGISTRY_TOKEN is not set/);
    });

    it('never prints a token', (t) => {
      const result = preflight(t, { GITHUB_REF: `refs/tags/${TAG}`, ...TOKENS }, NPM_ACCEPTS_TOKEN);
      assert.equal(result.status, 0, result.stderr);
      assert.doesNotMatch(result.stdout + result.stderr + result.output, /-secret/);
    });

    it('fails when the tag is not the workspace version', (t) => {
      const result = preflight(t, { GITHUB_REF: 'refs/tags/v99.0.0', ...TOKENS });
      assert.equal(result.status, 1);
      assert.match(result.stderr, new RegExp(`v99\\.0\\.0 does not match version ${VERSION}`));
    });

    it('fails when the versions drift', (t) => {
      const root = tempDir(t);
      fs.mkdirSync(path.join(root, '.github', 'scripts'), { recursive: true });
      fs.copyFileSync(path.join(SCRIPTS, 'preflight.sh'), path.join(root, '.github', 'scripts', 'preflight.sh'));
      fs.copyFileSync(path.join(REPO, 'Cargo.toml'), path.join(root, 'Cargo.toml'));
      fs.mkdirSync(path.join(root, 'npm'));
      fs.copyFileSync(path.join(NPM_DIR, 'check-versions.js'), path.join(root, 'npm', 'check-versions.js'));
      for (const name of ['kbx', ...TARGETS.map((row) => row.pkg)]) {
        fs.mkdirSync(path.join(root, 'npm', name));
        fs.writeFileSync(path.join(root, 'npm', name, 'package.json'), '{"version":"0.0.0"}');
      }
      const result = preflight(t, {}, { scripts: path.join(root, '.github', 'scripts') });
      assert.equal(result.status, 1);
      assert.match(result.stderr, /check-versions/);
      assert.doesNotMatch(result.output, /^publish=/m);
    });
  });

  describe('publish-npm.sh', () => {
    const relative = 'dist/kbx-darwin-arm64-1.2.3.tgz';
    const args = ['kbx-darwin-arm64', '1.2.3', relative];

    // A checkout with the tarball in it, given to the script as release.yml does:
    // relative to the working directory.
    function checkout(t) {
      const cwd = fs.realpathSync(tempDir(t));
      fs.mkdirSync(path.join(cwd, 'dist'));
      fs.writeFileSync(path.join(cwd, relative), '');
      return { cwd, tarball: path.join(cwd, relative) };
    }

    it('publishes the tarball when the package has never been published', (t) => {
      const { cwd, tarball } = checkout(t);
      const view = 'if [[ $1 == view ]]; then echo "npm error code E404" >&2; exit 1; fi';
      const result = run(t, 'publish-npm.sh', args, { cwd, tools: { npm: view } });
      assert.equal(result.status, 0, result.stderr);
      assert.deepEqual(result.calls, ['npm view kbx-darwin-arm64@1.2.3 version', `npm publish ${tarball}`]);
    });

    it('never gives npm a path it can read as a GitHub repository', (t) => {
      const { cwd } = checkout(t);
      const result = run(t, 'publish-npm.sh', args, { cwd, tools: { npm: 'exit 0' } });
      assert.equal(result.status, 0, result.stderr);
      assert.ok(path.isAbsolute(result.calls[1].split(' ')[2]), result.calls[1]);
    });

    it('publishes when npm view finds nothing but exits 0', (t) => {
      const { cwd } = checkout(t);
      const result = run(t, 'publish-npm.sh', args, { cwd, tools: { npm: 'exit 0' } });
      assert.equal(result.status, 0, result.stderr);
      assert.equal(result.calls.length, 2);
      assert.match(result.calls[1], /^npm publish /);
    });

    it('skips a version that is already published', (t) => {
      const { cwd } = checkout(t);
      const view = 'if [[ $1 == view ]]; then echo 1.2.3; fi';
      const result = run(t, 'publish-npm.sh', args, { cwd, tools: { npm: view } });
      assert.equal(result.status, 0, result.stderr);
      assert.deepEqual(result.calls, ['npm view kbx-darwin-arm64@1.2.3 version']);
      assert.match(result.stdout, /already published/);
    });

    it('stops without publishing when the registry cannot be read', (t) => {
      const { cwd } = checkout(t);
      const view = 'if [[ $1 == view ]]; then echo "npm error code ETIMEDOUT" >&2; exit 1; fi';
      const result = run(t, 'publish-npm.sh', args, { cwd, tools: { npm: view } });
      assert.equal(result.status, 1);
      assert.equal(result.calls.length, 1);
      assert.match(result.stderr, /ETIMEDOUT/);
    });

    it('fails when the publish fails', (t) => {
      const { cwd } = checkout(t);
      const npm = 'if [[ $1 == publish ]]; then exit 1; fi';
      assert.equal(run(t, 'publish-npm.sh', args, { cwd, tools: { npm } }).status, 1);
    });

    it('fails before calling npm when the tarball is missing', (t) => {
      const result = run(t, 'publish-npm.sh', args, { cwd: tempDir(t), tools: { npm: 'exit 0' } });
      assert.equal(result.status, 1);
      assert.deepEqual(result.calls, []);
      assert.match(result.stderr, /no tarball at dist\/kbx-darwin-arm64-1\.2\.3\.tgz/);
    });

    it('takes a dry run through npm publish, even for a version already out', (t) => {
      const { cwd, tarball } = checkout(t);
      const view = 'if [[ $1 == view ]]; then echo 1.2.3; fi';
      const result = run(t, 'publish-npm.sh', [...args, '--dry-run'], { cwd, tools: { npm: view } });
      assert.equal(result.status, 0, result.stderr);
      assert.deepEqual(result.calls, [`npm publish --dry-run ${tarball}`]);
    });

    it('fails a dry run when npm does', (t) => {
      const { cwd } = checkout(t);
      const result = run(t, 'publish-npm.sh', [...args, '--dry-run'], { cwd, tools: { npm: 'exit 128' } });
      assert.equal(result.status, 128);
    });

    it('refuses to publish for real on a mistyped dry run flag', (t) => {
      const { cwd } = checkout(t);
      const result = run(t, 'publish-npm.sh', [...args, '--dryrun'], { cwd, tools: { npm: 'exit 0' } });
      assert.equal(result.status, 1);
      assert.deepEqual(result.calls, []);
      assert.match(result.stderr, /usage: publish-npm\.sh/);
    });
  });

  describe('publish-crate.sh', () => {
    const line = (version) => `{"name":"kbx-core","vers":"${version}","deps":[]}`;
    // A fake curl that answers the -w format's status code and writes the
    // index file to the path after --output.
    const curl = (status, body = '') => `
      while [[ $# -gt 0 ]]; do [[ $1 == --output ]] && out=$2; shift; done
      printf '%s\\n' '${body}' > "$out"
      printf '${status}'`;

    it('publishes, then waits for the version to reach the index', (t) => {
      const dir = tempDir(t);
      const flag = path.join(dir, 'published');
      const tools = {
        cargo: `touch ${flag}`,
        curl: `
          while [[ $# -gt 0 ]]; do [[ $1 == --output ]] && out=$2; shift; done
          count=$(grep -c '^curl' "$CALLS")
          if [[ -f ${flag} && $count -ge 3 ]]; then echo '${line('1.2.3')}' > "$out"; printf 200
          else : > "$out"; printf 404; fi`,
      };
      const result = run(t, 'publish-crate.sh', ['kbx-core', '1.2.3'], { tools });
      assert.equal(result.status, 0, result.stderr);
      assert.deepEqual(
        result.calls.map((call) => call.split(' ')[0]),
        ['curl', 'cargo', 'curl', 'curl'],
      );
      assert.equal(result.calls[1], 'cargo publish --locked --package kbx-core');
    });

    it('skips a version already in the index', (t) => {
      const tools = { cargo: 'exit 1', curl: curl(200, `${line('1.2.2')}\n${line('1.2.3')}`) };
      const result = run(t, 'publish-crate.sh', ['kbx-core', '1.2.3'], { tools });
      assert.equal(result.status, 0, result.stderr);
      assert.equal(result.calls.filter((call) => call.startsWith('cargo')).length, 0);
      assert.match(result.stdout, /already published/);
    });

    it('does not mistake another version for this one', (t) => {
      const tools = { cargo: '', curl: curl(200, line('1.2.3-rc.1')) };
      const result = run(t, 'publish-crate.sh', ['kbx-core', '1.2.3'], {
        tools,
        env: { KBX_CRATE_POLL_ATTEMPTS: '1' },
      });
      assert.equal(result.calls.filter((call) => call.startsWith('cargo')).length, 1);
    });

    for (const [crate, file] of [
      ['kbx', '3/k/kbx'],
      ['kbx-core', 'kb/x-/kbx-core'],
      ['kbx-judge', 'kb/x-/kbx-judge'],
    ]) {
      it(`reads ${crate} from ${file} in the sparse index`, (t) => {
        const tools = { cargo: 'exit 1', curl: curl(200, '{"vers":"1.2.3"}') };
        const result = run(t, 'publish-crate.sh', [crate, '1.2.3'], { tools });
        assert.equal(result.status, 0, result.stderr);
        assert.match(result.calls[0], new RegExp(`https://index\\.crates\\.io/${file}( |$)`));
      });
    }

    it('fails when the version never reaches the index', (t) => {
      const tools = { cargo: '', curl: curl(404) };
      const result = run(t, 'publish-crate.sh', ['kbx-core', '1.2.3'], {
        tools,
        env: { KBX_CRATE_POLL_ATTEMPTS: '2' },
      });
      assert.equal(result.status, 1);
      assert.match(result.stderr, /kbx-core 1\.2\.3 is not visible/);
    });

    it('stops without publishing when the index cannot be read', (t) => {
      const tools = { cargo: '', curl: curl(503) };
      const result = run(t, 'publish-crate.sh', ['kbx-core', '1.2.3'], { tools });
      assert.equal(result.status, 1);
      assert.equal(result.calls.filter((call) => call.startsWith('cargo')).length, 0);
      assert.match(result.stderr, /503/);
    });

    it('fails when cargo publish fails', (t) => {
      const tools = { cargo: 'exit 101', curl: curl(404) };
      assert.notEqual(run(t, 'publish-crate.sh', ['kbx-core', '1.2.3'], { tools }).status, 0);
    });
  });

  describe('publish-github.sh', () => {
    const files = ['binaries/kbx-darwin-arm64', 'binaries/kbx-windows-x64.exe'];

    it('creates the release and attaches every file', (t) => {
      const gh = 'if [[ $2 == view ]]; then echo "release not found" >&2; exit 1; fi';
      const result = run(t, 'publish-github.sh', ['v1.2.3', ...files], { tools: { gh } });
      assert.equal(result.status, 0, result.stderr);
      assert.deepEqual(result.calls.slice(1), [
        'gh release create v1.2.3 --verify-tag --generate-notes --title v1.2.3',
        'gh release upload v1.2.3 binaries/kbx-darwin-arm64',
        'gh release upload v1.2.3 binaries/kbx-windows-x64.exe',
      ]);
    });

    it('attaches only what an existing release lacks', (t) => {
      const gh = 'if [[ $2 == view ]]; then echo kbx-darwin-arm64; fi';
      const result = run(t, 'publish-github.sh', ['v1.2.3', ...files], { tools: { gh } });
      assert.equal(result.status, 0, result.stderr);
      assert.deepEqual(result.calls.slice(1), ['gh release upload v1.2.3 binaries/kbx-windows-x64.exe']);
    });

    it('does nothing when the release is complete', (t) => {
      const gh = 'if [[ $2 == view ]]; then echo kbx-darwin-arm64; echo kbx-windows-x64.exe; fi';
      const result = run(t, 'publish-github.sh', ['v1.2.3', ...files], { tools: { gh } });
      assert.equal(result.status, 0, result.stderr);
      assert.equal(result.calls.length, 1);
    });

    it('stops when GitHub cannot be read', (t) => {
      const gh = 'if [[ $2 == view ]]; then echo "HTTP 502" >&2; exit 1; fi';
      const result = run(t, 'publish-github.sh', ['v1.2.3', ...files], { tools: { gh } });
      assert.equal(result.status, 1);
      assert.equal(result.calls.length, 1);
      assert.match(result.stderr, /HTTP 502/);
    });
  });
});

describe('release.yml', () => {
  const workflow = fs.readFileSync(path.join(REPO, '.github', 'workflows', 'release.yml'), 'utf8');

  function needs(job) {
    const block = workflow.split(/^  (?=[\w-]+:\r?$)/m).find((text) => text.startsWith(`${job}:`));
    assert.ok(block, `no ${job} job`);
    const match = block.match(/^ {4}needs: (.+?)\r?$/m);
    return match ? match[1].replace(/[[\]]/g, '').split(/,\s*/) : [];
  }

  function upstream(job) {
    return needs(job).flatMap((name) => [name, ...upstream(name)]);
  }

  it('builds one binary per npm platform package', () => {
    const packages = [...workflow.matchAll(/^ +package: (\S+)\r?$/gm)].map((match) => match[1]);
    assert.deepEqual(packages.sort(), TARGETS.map((row) => row.pkg).sort());
  });

  it('publishes to npm and crates.io independently', () => {
    assert.ok(!upstream('cargo-publish').includes('npm-publish'));
    assert.ok(!upstream('npm-publish').includes('cargo-publish'));
  });

  it('holds both registries back until the GitHub Release is out', () => {
    assert.ok(needs('cargo-publish').includes('github-release'));
    assert.ok(needs('npm-publish').includes('github-release'));
  });
});
