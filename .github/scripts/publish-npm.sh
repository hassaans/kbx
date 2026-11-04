#!/usr/bin/env bash
# Publishes a tarball from npm/pack.sh, unless the registry already has that
# version, so a re-run after a partial failure skips what went out. --dry-run
# runs `npm publish --dry-run` whatever the registry has, so the check still
# means something once the version is out.
set -euo pipefail

usage="usage: publish-npm.sh <package> <version> <tarball> [--dry-run]"
pkg=${1:?$usage}
version=${2:?$usage}
tarball=${3:?$usage}
mode=${4:-}

if [[ -n $mode && $mode != --dry-run ]]; then
  echo "$usage" >&2
  exit 1
fi

if [[ ! -f $tarball ]]; then
  echo "publish-npm: no tarball at $tarball" >&2
  exit 1
fi
# npm reads a bare dist/x.tgz as the GitHub repository dist/x.tgz.
tarball=$(realpath "$tarball")

if [[ -n $mode ]]; then
  npm publish --dry-run "$tarball"
  exit 0
fi

errors=$(mktemp)
trap 'rm -f "$errors"' EXIT

# npm reports a missing version as E404. Anything else means we can't tell.
if published=$(npm view "$pkg@$version" version 2>"$errors"); then
  if [[ $published == "$version" ]]; then
    echo "$pkg@$version is already published, skipping"
    exit 0
  fi
elif ! grep -q E404 "$errors"; then
  cat "$errors" >&2
  exit 1
fi

npm publish "$tarball"
