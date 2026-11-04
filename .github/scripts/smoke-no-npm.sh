#!/usr/bin/env bash
# Runs a published kbx doctor where npm and Node can't be found, and fails unless
# it refuses with the message that names Node >= 22. It runs the binary from the
# platform package, because the npm shim needs Node.
set -euo pipefail

usage="usage: smoke-no-npm.sh <version> <platform package>"
version=${1:?$usage}
package=${2:?$usage}

fail() {
  echo "smoke-no-npm: $*" >&2
  exit 1
}

work=$(mktemp -d)
cd "$work"
tar -xzf "$(npm pack "$package@$version" --silent)"
binary=$work/package/bin/kbx

# Every other folder stays, so doctor sees a normal machine.
clean=
IFS=: read -ra folders <<<"$PATH"
for folder in "${folders[@]}"; do
  if [[ ! -e $folder/npm && ! -e $folder/node && ! -e $folder/qmd ]]; then
    clean+=${clean:+:}$folder
  fi
done

status=0
env PATH="$clean" HOME="$work" "$binary" doctor </dev/null >out 2>err || status=$?
echo "--- kbx doctor without npm (exit $status)"
cat out err

[[ $status == 1 ]] || fail "doctor exited $status, not 1"
grep -Eq '^qmd +fail +not installed' out || fail "doctor printed no failed qmd row"
grep -qF 'Node >= 22' err || fail "doctor's error does not name Node >= 22"
