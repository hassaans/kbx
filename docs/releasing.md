# Releasing kbx

Pushing a `v*` tag runs `.github/workflows/release.yml`. It builds the five
targets, tests each one on its own platform, and then publishes in this order:

1. a GitHub Release with the five binaries attached
2. the five platform packages on npm
3. the `kbx` shim on npm
4. `kbx-core`, `kbx-gen`, `kbx-judge` and `kbx` on crates.io, waiting for each
   crate to reach the index before the next

Nothing publishes unless every build, test and packaging step passes.

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
   - the five `optionalDependencies` pins in `npm/kbx/package.json`
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

A dry run does everything except the publish jobs. That includes `npm publish
--dry-run` on all six tarballs, through the same script and with the same paths
as the real publish. It runs by itself on a pull request that changes
`release.yml`, `.github/scripts/` or `npm/pack.sh`. To start one on any branch:

```sh
gh workflow run release.yml --ref <branch>
```

A manual run only publishes if it's started on a tag with `dry_run` turned off.

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
  run picks up from the first package or crate that's missing.
- **A bug that needs a code change, after something published**: npm and
  crates.io versions can't be replaced, and a re-run uses the tagged commit, so
  re-running won't help. Fix it on `main` and release the next patch version.
  Leave the tag and the versions already out as they are. Remove a bad version
  with `cargo yank --version <version> <crate>`, and with `npm deprecate` on npm.
- **The GitHub Release published but npm did not**: this is the same case, even
  though neither registry has the version. Leave the tag and the Release as they
  are, and don't delete them to reuse the version. Fix it on `main` and release
  the next patch version. v0.1.0 went this way: it's a GitHub Release with no npm
  packages or crates, and v0.1.1 is the first version on the registries.

A re-run can't pick up an edit to `release.yml` or `.github/scripts/` either: it
always uses the files at the tag.
