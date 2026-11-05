'use strict';

const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const { describe, it } = require('node:test');

const { NPM_DIR, REPO, TARGETS, tempDir } = require('./helpers');

const SCRIPTS = path.join(REPO, '.github', 'scripts');
const VERSION = require('../kbx-cli/package.json').version;
const TAG = `v${VERSION}`;

// Every tool that can publish. A test that doesn't fake one gets a stub that
// fails, so these tests can never reach a real registry.
const PUBLISHERS = { cargo: 'exit 97', curl: 'exit 97', gh: 'exit 97', npm: 'exit 97' };

// Puts fake tools on PATH, given as { name: bash body }, and runs a release
// script with them. Returns the result plus each fake's calls, one per line.
function run(t, script, args, { tools = {}, env = {}, scripts = SCRIPTS, cwd, inheritedPath = process.env.PATH } = {}) {
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
      PATH: `${dir}${path.delimiter}${inheritedPath}`,
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
      for (const name of ['kbx-cli', ...TARGETS.map((row) => row.pkg)]) {
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

  describe('smoke scripts', () => {
    const PLATFORM = 'kbx-linux-x64-gnu';
    const VAULT = path.join(REPO, 'tests', 'fixtures', 'vault');
    const ROW = 'printf "%-14s%-5s%s\\n"';

    // A published kbx that behaves, with FAKE_* in the environment for the
    // one way a test wants it to misbehave. npm installs it as `kbx`.
    const FAKE_KBX = `
      here=$(dirname "$0")
      case $1 in
        --version) echo "kbx \${FAKE_VERSION:-1.2.3}" ;;
        doctor)
          [[ $FAKE_DOCTOR != noisy ]] || echo "added 300 packages" >&2
          if [[ $FAKE_DOCTOR == fails ]]; then ${ROW} qmd fail "not installed"; exit 1; fi
          if [[ $FAKE_DOCTOR == unhealthy ]]; then ${ROW} qmd ok "$here/qmd"; exit 1; fi
          if [[ $FAKE_DOCTOR == moves && -e $here/qmd ]]; then ${ROW} qmd ok /elsewhere/qmd; exit 0; fi
          if [[ $FAKE_DOCTOR != skips ]]; then touch "$here/qmd"; chmod +x "$here/qmd"; fi
          if [[ -f kbx.toml ]]; then
            ${ROW} vault ok "$PWD"
            ${ROW} index "\${FAKE_INDEX_ROW:-ok}" "25 notes, 58 edges, 29 sections, 2 hidden links"
          fi
          ${ROW} qmd ok "$here/qmd"
          [[ ! -f kbx.toml ]] || ${ROW} collections "\${FAKE_COLLECTIONS_ROW:-ok}" "\${FAKE_COLLECTIONS:-4} in index kbx-abc"
          ;;
        init)
          touch kbx.toml
          [[ $FAKE_INIT != fails ]] || exit 1
          if [[ $FAKE_INIT == warns ]]; then echo "warning: qmd collections not created" >&2; exit 0; fi
          for scope in decisions engineering lint operations; do
            [[ $scope == "$FAKE_INIT_SKIPS" ]] || echo "created qmd collection $scope"
          done
          ;;
        index) echo "\${FAKE_INDEXED:-indexed 25, 0 unchanged, removed 0, 2 skipped, 2 failed, 2 hidden links}" ;;
        lint)
          echo "\${FAKE_LINT:-12 errors, 1 warning}"
          exit "\${FAKE_LINT_STATUS:-1}"
          ;;
      esac`;

    const FAKE_NPM = `
      here=$(dirname "$0")
      case "$1 $2" in
        'i -g')
          cp "$here/fake-kbx" "$here/kbx"
          [[ -z $FAKE_INSTALL_FAILS ]] || exit 1
          mkdir -p "$here/root/kbx-cli"
          parent=$here/root/kbx-cli/node_modules
          [[ -z $FAKE_HOISTED ]] || parent=$here/root
          for pkg in \${FAKE_PLATFORMS-${PLATFORM}}; do mkdir -p "$parent/$pkg"; done
          ;;
        'root -g') echo "$here/root" ;;
        'ls -g') [[ -e $here/qmd ]] ;;
        *) exit 97 ;;
      esac`;

    // A developer's machine may well have the real ones, which the script would
    // rightly refuse.
    const CLEAN_PATH = process.env.PATH.split(path.delimiter)
      .filter((folder) => !['kbx', 'qmd'].some((tool) => fs.existsSync(path.join(folder, tool))))
      .join(path.delimiter);

    function smokeNpm(t, env = {}, tools = {}) {
      return run(t, 'smoke-npm.sh', ['1.2.3', PLATFORM, VAULT], {
        tools: { npm: FAKE_NPM, 'fake-kbx': FAKE_KBX, ...tools },
        env,
        inheritedPath: CLEAN_PATH,
      });
    }

    it('expects platform packages that exist', () => {
      const workflow = fs.readFileSync(path.join(REPO, '.github', 'workflows', 'smoke.yml'), 'utf8');
      const expected = [...workflow.matchAll(/package: (\S+)/g)].map((match) => match[1]);
      assert.deepEqual(expected, ['kbx-linux-x64-gnu', 'kbx-darwin-arm64', 'kbx-windows-x64']);
      for (const pkg of expected) assert.ok(TARGETS.some((row) => row.pkg === pkg), pkg);
    });

    it('waits for the shim on npm and for kbx on crates.io', () => {
      const workflow = fs.readFileSync(path.join(REPO, '.github', 'workflows', 'smoke.yml'), 'utf8');
      const waits = [...workflow.matchAll(/smoke-wait\.sh (\S+) (\S+) /g)].map((match) => `${match[1]} ${match[2]}`);
      assert.deepEqual(waits, ['npm kbx-cli', `npm ${PLATFORM}`, 'crates kbx']);
    });

    describe('smoke-npm.sh', () => {
      it('passes on a good install, and prints both doctor runs', (t) => {
        const result = smokeNpm(t);
        assert.equal(result.status, 0, result.stderr);
        assert.equal(result.calls[0], 'npm i -g kbx-cli@1.2.3');
        assert.equal(result.stdout.match(/^qmd +ok +/gm).length, 3);
        assert.match(result.stdout, /kbx doctor, first run/);
        assert.match(result.stdout, /kbx doctor, second run/);
      });

      it('finds a platform package npm hoisted next to the shim', (t) => {
        const result = smokeNpm(t, { FAKE_HOISTED: '1' });
        assert.equal(result.status, 0, result.stderr);
        assert.match(result.stdout, /platform package: .*\/root\/kbx-linux-x64-gnu$/m);
      });

      it('runs doctor before init, because init would install qmd', (t) => {
        const result = smokeNpm(t);
        const order = result.calls.filter((call) => /^fake-kbx (doctor|init)/.test(call));
        assert.deepEqual(order.slice(0, 3).map((call) => call.split(' ')[1]), ['doctor', 'doctor', 'init']);
      });

      // Each row: what goes wrong, and the words the job must fail with.
      const FAILURES = [
        ['no platform package is installed', { FAKE_PLATFORMS: '' }, /expected one platform package, found 0/],
        [
          'two platform packages are installed',
          { FAKE_PLATFORMS: `${PLATFORM} kbx-darwin-arm64` },
          /expected one platform package, found 2/,
        ],
        ['the wrong platform package is installed', { FAKE_PLATFORMS: 'kbx-darwin-arm64' }, /expected kbx-linux-x64-gnu/],
        ['kbx reports another version', { FAKE_VERSION: '1.2.2' }, /kbx --version printed kbx 1\.2\.2/],
        ['the first doctor fails', { FAKE_DOCTOR: 'fails' }, /first doctor exited 1/],
        ['doctor exits non-zero with qmd ok', { FAKE_DOCTOR: 'unhealthy' }, /first doctor exited 1/],
        ['the install leaks into doctor\'s output', { FAKE_DOCTOR: 'noisy' }, /first doctor wrote to stderr/],
        ['doctor says ok but installs nothing', { FAKE_DOCTOR: 'skips' }, /qmd is not on PATH after the first doctor/],
        ['the second doctor reports another qmd', { FAKE_DOCTOR: 'moves' }, /second doctor found \/elsewhere\/qmd/],
        ['init fails', { FAKE_INIT: 'fails' }, /kbx init exited 1/],
        ['init only warns about collections', { FAKE_INIT: 'warns' }, /no qmd collection for decisions/],
        ['init misses one scope', { FAKE_INIT_SKIPS: 'lint' }, /no qmd collection for lint/],
        ['doctor warns about collections', { FAKE_COLLECTIONS_ROW: 'warn' }, /did not report 4 collections/],
        ['doctor counts other collections', { FAKE_COLLECTIONS: '3' }, /did not report 4 collections/],
        ['index compiles the wrong notes', { FAKE_INDEXED: 'indexed 0, 0 unchanged' }, /kbx index printed/],
        ['lint finds nothing', { FAKE_LINT: '0 errors, 0 warnings', FAKE_LINT_STATUS: '0' }, /kbx lint exited 0/],
        ['lint finds other errors', { FAKE_LINT: '11 errors, 1 warning' }, /kbx lint printed/],
        ['doctor does not see the index', { FAKE_INDEX_ROW: 'warn' }, /doctor in the vault/],
      ];

      for (const [name, env, message] of FAILURES) {
        it(`fails when ${name}`, (t) => {
          const result = smokeNpm(t, env);
          assert.notEqual(result.status, 0, result.stdout);
          assert.match(result.stderr, message);
        });
      }

      it('fails when the install fails', (t) => {
        const result = smokeNpm(t, { FAKE_INSTALL_FAILS: '1' });
        assert.equal(result.status, 1);
        assert.doesNotMatch(result.stdout, /kbx doctor/);
      });

      it('fails on a machine that already has kbx', (t) => {
        const result = smokeNpm(t, {}, { kbx: '' });
        assert.equal(result.status, 1);
        assert.match(result.stderr, /kbx is already on this machine/);
        assert.equal(result.calls.filter((call) => call.startsWith('npm i')).length, 0);
      });

      it('fails on a machine that already has qmd', (t) => {
        const result = smokeNpm(t, {}, { qmd: '' });
        assert.equal(result.status, 1);
        assert.match(result.stderr, /qmd is already on this machine/);
      });
    });

    describe('smoke-no-npm.sh', () => {
      const MESSAGE =
        'error: qmd is not installed, and installing it needs Node >= 22 and npm, which are not on PATH.';

      // npm pack gives a tarball whose bin/kbx is the given script. The script
      // sees the PATH that the check left it.
      function noNpm(t, binary) {
        const packNpm = `
          [[ $1 == pack && $2 == ${PLATFORM}@1.2.3 ]] || exit 97
          mkdir -p package/bin
          cat > package/bin/kbx <<'BINARY'
#!/bin/bash
${binary}
BINARY
          chmod +x package/bin/kbx
          tar -czf out.tgz package
          rm -r package
          echo out.tgz`;
        return run(t, 'smoke-no-npm.sh', ['1.2.3', PLATFORM], { tools: { npm: packNpm } });
      }

      const ROW_FAIL = `printf 'qmd           fail not installed\\n'`;

      it('passes when doctor fails naming Node >= 22', (t) => {
        const result = noNpm(t, `${ROW_FAIL}; echo '${MESSAGE}' >&2; exit 1`);
        assert.equal(result.status, 0, result.stderr);
        assert.match(result.stdout, /Node >= 22/);
      });

      it('takes every npm and node off the PATH doctor sees', (t) => {
        const script = `
          if type -P npm || type -P node; then echo found >&2; exit 0; fi
          ${ROW_FAIL}; echo '${MESSAGE}' >&2; exit 1`;
        const result = noNpm(t, script);
        assert.equal(result.status, 0, result.stderr);
      });

      it('fails when doctor succeeds', (t) => {
        const result = noNpm(t, `${ROW_FAIL}; echo '${MESSAGE}' >&2; exit 0`);
        assert.equal(result.status, 1);
        assert.match(result.stderr, /doctor exited 0/);
      });

      it('fails when the message does not name Node >= 22', (t) => {
        const result = noNpm(t, `${ROW_FAIL}; echo 'error: no qmd' >&2; exit 1`);
        assert.equal(result.status, 1);
        assert.match(result.stderr, /does not name Node >= 22/);
      });

      it('fails when the qmd row is not a failure', (t) => {
        const result = noNpm(t, `echo '${MESSAGE}' >&2; exit 1`);
        assert.equal(result.status, 1);
        assert.match(result.stderr, /no failed qmd row/);
      });
    });

    describe('smoke-wait.sh', () => {
      const FAST = { KBX_SMOKE_POLL_ATTEMPTS: '3', KBX_SMOKE_POLL_SECONDS: '0' };

      it('returns once npm has the version', (t) => {
        const npm = `[[ $(grep -c '^npm' "$CALLS") -ge 2 ]] && echo 1.2.3 || exit 1`;
        const result = run(t, 'smoke-wait.sh', ['npm', 'kbx-cli', '1.2.3'], { tools: { npm }, env: FAST });
        assert.equal(result.status, 0, result.stderr);
        assert.deepEqual(result.calls, ['npm view kbx-cli@1.2.3 version', 'npm view kbx-cli@1.2.3 version']);
      });

      it('fails when npm never has the version', (t) => {
        const result = run(t, 'smoke-wait.sh', ['npm', 'kbx-cli', '1.2.3'], {
          tools: { npm: 'echo 1.2.2' },
          env: FAST,
        });
        assert.equal(result.status, 1);
        assert.equal(result.calls.length, 3);
        assert.match(result.stderr, /kbx-cli 1\.2\.3 is not on npm/);
      });

      it('returns once the crates.io index has the version', (t) => {
        const curl = `echo '{"name":"kbx","vers":"1.2.3"}'`;
        const result = run(t, 'smoke-wait.sh', ['crates', 'kbx', '1.2.3'], { tools: { curl }, env: FAST });
        assert.equal(result.status, 0, result.stderr);
        assert.match(result.calls[0], /https:\/\/index\.crates\.io\/3\/k\/kbx/);
      });

      it('does not mistake another crate version for this one', (t) => {
        const curl = `echo '{"name":"kbx","vers":"1.2.3-rc.1"}'`;
        const result = run(t, 'smoke-wait.sh', ['crates', 'kbx', '1.2.3'], { tools: { curl }, env: FAST });
        assert.equal(result.status, 1);
        assert.match(result.stderr, /kbx 1\.2\.3 is not on crates/);
      });

      it('refuses an unknown registry', (t) => {
        const result = run(t, 'smoke-wait.sh', ['pypi', 'kbx', '1.2.3'], { env: FAST });
        assert.equal(result.status, 1);
        assert.match(result.stderr, /usage/);
      });
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

  it('packs the shim, then dry-runs and publishes it after the platform packages', () => {
    assert.match(workflow, /^ +npm\/pack\.sh kbx-cli dist\r?$/m);
    const loops = [...workflow.matchAll(/^ +for pkg in (.+); do\r?$/gm)].map((match) => match[1].split(' '));
    assert.equal(loops.length, 2);
    for (const packages of loops) {
      assert.deepEqual(packages, [...TARGETS.map((row) => row.pkg), 'kbx-cli']);
    }
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
