#!/usr/bin/env bash
# Creates the GitHub Release for a tag and attaches files to it, skipping
# whatever a previous run already created or attached.
set -euo pipefail

usage="usage: publish-github.sh <tag> <file>..."
tag=${1:?$usage}
shift

errors=$(mktemp)
trap 'rm -f "$errors"' EXIT

if ! attached=$(gh release view "$tag" --json assets --jq '.assets[].name' 2>"$errors"); then
  if ! grep -q 'release not found' "$errors"; then
    cat "$errors" >&2
    exit 1
  fi
  gh release create "$tag" --verify-tag --generate-notes --title "$tag"
  attached=
fi

for file in "$@"; do
  name=$(basename "$file")
  if grep -qxF "$name" <<<"$attached"; then
    echo "$name is already attached, skipping"
  else
    gh release upload "$tag" "$file"
  fi
done
