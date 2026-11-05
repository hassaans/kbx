#!/usr/bin/env bash
# Waits for a version to reach npm or crates.io. A registry can take a while to
# show what was just published, and a smoke run shouldn't fail for being early.
set -euo pipefail

usage="usage: smoke-wait.sh npm|crates <package> <version>"
registry=${1:?$usage}
package=${2:?$usage}
version=${3:?$usage}

attempts=${KBX_SMOKE_POLL_ATTEMPTS:-60}
delay=${KBX_SMOKE_POLL_SECONDS:-30}

case $registry in
  npm)
    visible() {
      [[ $(npm view "$package@$version" version 2>/dev/null) == "$version" ]]
    }
    ;;
  crates)
    # The sparse index layout for a name of three characters, which kbx is.
    visible() {
      curl --silent --show-error --fail --retry 3 "https://index.crates.io/3/${package:0:1}/$package" 2>/dev/null |
        grep -qF "\"vers\":\"$version\""
    }
    ;;
  *)
    echo "$usage" >&2
    exit 1
    ;;
esac

for ((attempt = 0; attempt < attempts; attempt++)); do
  if visible; then
    echo "$package $version is on $registry"
    exit 0
  fi
  sleep "$delay"
done

echo "smoke-wait: $package $version is not on $registry after $((attempts * delay))s" >&2
exit 1
