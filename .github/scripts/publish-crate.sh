#!/usr/bin/env bash
# Publishes one workspace crate and waits until it's in the crates.io index, so
# the next crate can resolve it. Skips a version the index already has, so a
# re-run after a partial failure skips what went out.
set -euo pipefail

usage="usage: publish-crate.sh <crate> <version>"
crate=${1:?$usage}
version=${2:?$usage}

attempts=${KBX_CRATE_POLL_ATTEMPTS:-60}
delay=${KBX_CRATE_POLL_SECONDS:-5}

# The sparse index layout: names of three characters go under 3/<first>, longer
# ones under their first two pairs of characters.
if [[ ${#crate} -eq 3 ]]; then
  url=https://index.crates.io/3/${crate:0:1}/$crate
else
  url=https://index.crates.io/${crate:0:2}/${crate:2:2}/$crate
fi

index=$(mktemp)
trap 'rm -f "$index"' EXIT

# Succeeds if the index lists the version. The index is compact JSON, one
# version per line. Exits on anything but a clear yes or no.
visible() {
  local status
  status=$(curl --silent --show-error --retry 3 --output "$index" --write-out '%{http_code}' "$url")
  case $status in
    200) grep -qF "\"vers\":\"$version\"" "$index" ;;
    404) return 1 ;;
    *)
      echo "publish-crate: $url answered $status" >&2
      exit 1
      ;;
  esac
}

if visible; then
  echo "$crate $version is already published, skipping"
  exit 0
fi

cargo publish --locked --package "$crate"

for ((attempt = 0; attempt < attempts; attempt++)); do
  if visible; then
    echo "$crate $version is visible on crates.io"
    exit 0
  fi
  sleep "$delay"
done

echo "publish-crate: $crate $version is not visible on crates.io after $((attempts * delay))s" >&2
exit 1
