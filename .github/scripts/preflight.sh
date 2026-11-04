#!/usr/bin/env bash
# Decides whether this run publishes, and writes `version` and `publish` to
# $GITHUB_OUTPUT. Only a v* tag publishes, and never on a dry run. Fails before
# anything is built if the versions drift, the tag doesn't match them, or a
# publish run is missing a token.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

node "$root/npm/check-versions.js"
version=$(node -p "require(process.argv[1]).version" "$root/npm/kbx/package.json")

publish=false
if [[ $GITHUB_REF == refs/tags/* ]]; then
  tag=${GITHUB_REF#refs/tags/}
  if [[ $tag != "v$version" ]]; then
    echo "preflight: tag $tag does not match version $version in Cargo.toml" >&2
    exit 1
  fi
  [[ ${DRY_RUN:-} == true ]] || publish=true
fi

if [[ $publish == true ]]; then
  missing=0
  for secret in NPM_TOKEN CARGO_REGISTRY_TOKEN; do
    if [[ -z ${!secret:-} ]]; then
      echo "preflight: $secret is not set. Add it under Settings > Secrets and variables > Actions, then re-run this workflow." >&2
      missing=1
    fi
  done
  [[ $missing == 0 ]] || exit 1
fi

echo "preflight: version $version, publish=$publish"
{
  echo "version=$version"
  echo "publish=$publish"
} >> "$GITHUB_OUTPUT"
