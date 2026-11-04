#!/usr/bin/env bash
# Packs one npm package into <out-dir> and prints the tarball's path. Platform
# packages take the binary to ship. Packing happens in a staged copy, so a
# binary never lands in the checkout.
set -euo pipefail

usage="usage: npm/pack.sh <package> <out-dir> [binary]"
pkg=${1:?$usage}
out=${2:?$usage}
binary=${3:-}

npm_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
if [[ ! -f $npm_dir/$pkg/package.json ]]; then
  echo "pack.sh: unknown package: $pkg" >&2
  exit 1
fi

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cp -R "$npm_dir/$pkg/." "$stage/"

if [[ $pkg != kbx ]]; then
  if [[ ! -f $binary ]]; then
    echo "pack.sh: $pkg needs a binary, got: ${binary:-nothing}" >&2
    exit 1
  fi
  file=kbx
  [[ $pkg == kbx-win32-* ]] && file=kbx.exe
  mkdir -p "$stage/bin"
  cp "$binary" "$stage/bin/$file"
  chmod 755 "$stage/bin/$file"
fi

mkdir -p "$out"
out=$(cd "$out" && pwd)
tarball=$(cd "$stage" && npm pack --ignore-scripts --pack-destination "$out")
echo "$out/$tarball"
