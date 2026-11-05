# Releasing kbx

Pushing a `v*` tag runs `.github/workflows/release.yml`. It builds the five
targets, tests each one on its own platform, and then publishes a GitHub Release
with the five binaries attached. After that, two jobs run side by side:

- `npm-publish`: the five platform packages, then the `kbx-cli` shim
- `cargo-publish`: `kbx-core`, `kbx-gen`, `kbx-judge` and `kbx` on crates.io,
  waiting for each crate to reach the index before the next

Neither job waits for the other, so a failure on one registry doesn't stop the
other from publishing. Nothing publishes unless every build, test and packaging
step passes. Once both are done, the `smoke` job installs what went out. See
[Check a release](#check-a-release).

## Before the first release

Add two repository secrets under Settings > Secrets and variables > Actions. The
workflow fails in its first job, naming the secret, if either is missing, or if
npm refuses `NPM_TOKEN`. crates.io has no such check, so a bad
`CARGO_REGISTRY_TOKEN` only shows in `cargo-publish`: replace it and re-run.

- `NPM_TOKEN`: an npm token that can publish without a one-time password, and
  that can create new packages.
- `CARGO_REGISTRY_TOKEN`: a crates.io token with the `publish-new` and
  `publish-update` scopes. Once the crates exist, `publish-update` is enough.

## Cut a release

1. Set the new version in these places, on a branch:
   - `version` under `[workspace.package]` in `Cargo.toml`
   - the `kbx-core` dependency in `crates/kbx/Cargo.toml`, and any other
     dependency between workspace crates
   - `version` in the six `npm/*/package.json` files
   - the five `optionalDependencies` pins in `npm/kbx-cli/package.json`
2. Update `Cargo.lock`, and check that nothing has drifted:

   ```sh
   cargo update --workspace
   node npm/check-versions.js
   ```

3. Open a pull request and merge it to `main`.
4. Tag the merge commit and push the tag. It must be `v` plus the version in
   `Cargo.toml`, or the workflow stops in its first job.

   ```sh
   git switch main && git pull
   git tag v0.2.0
   git push origin v0.2.0
   ```

5. Watch the run with `gh run watch`.

Prereleases aren't handled: a version like `0.2.0-rc.1` would become npm's
`latest`.

## Try it without publishing

A dry run does everything except the publish jobs and the `smoke` job, which has
nothing to install. That includes `npm publish --dry-run` on all six tarballs,
through the same script and with the same paths as the real publish. It runs by
itself on a pull request that changes `release.yml`, `smoke.yml`,
`.github/scripts/` or `npm/pack.sh`. To start one on any branch:

```sh
gh workflow run release.yml --ref <branch>
```

A manual run only publishes if it's started on a tag with `dry_run` turned off.

## Check a release

`.github/workflows/smoke.yml` installs a published version on clean runners. On
Linux, macOS and Windows it installs `kbx-cli@<version>` from npm and checks that
exactly one platform package came with it. It tests the built-in default first:
`kbx init`, `kbx index`, `kbx lint`, a `kbx suggest` that downloads the real model
and `kbx doctor` on a copy of the fixture vault, none of which may install qmd.
Then it tests the QMD option: with `search = "qmd"`, `kbx doctor` runs twice to see
qmd installed and then found, and `kbx init --search qmd` has to create a qmd
collection per scope. Other jobs check that without npm and Node the default still
works and `kbx doctor` on `search = "qmd"` names Node >= 22, and that `cargo
install` works on macOS.

It runs at the end of every release run, as the `smoke` job of `release.yml`,
with the version from the tag. It waits for `npm-publish` and `cargo-publish`, and
each of its jobs then waits up to 30 minutes for its package to show on the
registry. A release that fails its smoke checks shows as a failed run, even though
everything is out.

`release.yml` calls it because nothing else would start it. The Release comes from
the workflow's `GITHUB_TOKEN`, and GitHub starts no workflows from events that
token causes, so a `release: published` trigger never fires for a tagged release.
v0.1.3 went this way, and its smoke run had to be started by hand. `smoke.yml` has
no such trigger any more. Publishing a Release by hand wouldn't need one: a new
tag starts `release.yml`, and an old tag has already been through it.

To run the checks by hand, for an older version or after a run that skipped them,
start them from that version's tag:

```sh
gh workflow run smoke.yml --ref v0.2.0 -f version=0.2.0
```

The `--ref` matters. The checks come from the ref and the packages from the
version, and the checks on `main` can expect behaviour an older version doesn't
have. v0.1.3 fails the ones on `main`, because they expect the built-in search
engine as the default.

## When a step fails midway

Every publish step checks what's already out and skips it, so re-running is safe
after any failure:

```sh
gh run rerun <run-id> --failed
```

What to do depends on where it stopped:

- **Before anything published** (`preflight`, `build`, `pack`, `package-crates`):
  nothing is out. If the fix is a secret, add it and re-run. If the fix is a code
  change, delete the tag, merge the fix and tag again:

  ```sh
  git push origin :refs/tags/v0.2.0
  git tag -d v0.2.0
  ```

- **A registry or network error during publishing**: re-run the failed jobs. The
  run picks up from the first package or crate that's missing. npm and crates.io
  publish independently, so one can be complete while the other failed: the
  re-run only touches the job that failed, and that job still skips what's out.
- **A bug that needs a code change, after something published**: npm and
  crates.io versions can't be replaced, and a re-run uses the tagged commit, so
  re-running won't help. Fix it on `main` and release the next patch version.
  Leave the tag and the versions already out as they are. Remove a bad version
  with `cargo yank --version <version> <crate>`, and with `npm deprecate` on npm.
- **The GitHub Release published but npm did not**: this is the same case, even
  though neither registry has the version. Leave the tag and the Release as they
  are, and don't delete them to reuse the version. Fix it on `main` and release
  the next patch version. v0.1.0 went this way: it's a GitHub Release with no npm
  packages or crates.
- **`npm-publish` or `cargo-publish` failed, so `smoke` was skipped**: re-running
  the failed jobs runs `smoke` too, once both have passed.
- **The `smoke` job failed**: everything is already out. If it's a network or
  registry error, re-run the failed jobs. If kbx is broken, treat it like any bug
  after publishing: fix it on `main` and release the next patch version. If the
  check itself is wrong, fix it on a branch and start it from there with
  `gh workflow run smoke.yml --ref <branch> -f version=<version>`.
- **npm refuses a package name**: no re-run gets past this, and the version stays
  partly out. Rename the package on `main` and release the next patch version.
  v0.1.1 went this way: npm answered `E403 Package name triggered spam detection`
  for `kbx-win32-x64-msvc`, from CI and from the owner's machine. So v0.1.1 is a
  GitHub Release plus four orphan platform packages (`kbx-darwin-arm64`,
  `kbx-darwin-x64`, `kbx-linux-x64-gnu` and `kbx-linux-arm64-gnu`). The `kbx` shim
  never went out, and neither did the crates, because `cargo-publish` ran after
  `npm-publish` then. The Windows package is `kbx-windows-x64` from v0.1.2.

  v0.1.2 went the same way: the GitHub Release, the four crates and the five
  platform packages are out at 0.1.2, but npm refused the shim with `E403 Package
  name too similar to existing packages ky,koa,mobx,knex,zx,rax,rx,sax,shx,tsx`.
  That's npm's typosquatting rule, which compares a new unscoped name with popular
  packages, so short names are the ones it catches. It can't be appealed from our
  side. So there's no `kbx` on npm, and the 0.1.2 platform packages have no shim
  that depends on them. The shim is `kbx-cli` from v0.1.3, and the command it
  installs is still `kbx`. v0.1.3 is the first version meant to reach both
  registries in full.

  Don't try a short unscoped name again. Check any new package name by hand too:
  the rule runs only on the registry during a real publish, so `npm publish
  --dry-run`, the release dry run and the tests can't catch it.

A re-run can't pick up an edit to `release.yml` or `.github/scripts/` either: it
always uses the files at the tag.
