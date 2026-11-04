#!/usr/bin/env bash
# Publishes a tarball from npm/pack.sh, unless the registry already has that
# version, so a re-run after a partial failure skips what went out.
set -euo pipefail

usage="usage: publish-npm.sh <package> <version> <tarball>"
pkg=${1:?$usage}
version=${2:?$usage}
tarball=${3:?$usage}

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
