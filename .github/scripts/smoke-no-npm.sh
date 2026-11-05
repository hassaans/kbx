#!/usr/bin/env bash
# Runs a published kbx where npm and Node can't be found. The default has to work,
# and `search = "qmd"` has to refuse with the message that names Node >= 22. It
# runs the binary from the platform package, because the npm shim needs Node.
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

# Leaves what kbx printed in $work/<name>.out and .err and its exit code in $status.
run() {
  local name=$1 title=$2
  shift 2
  status=0
  env PATH="$clean" HOME="$work" "$binary" "$@" </dev/null >"$work/$name.out" 2>"$work/$name.err" || status=$?
  echo "--- $title (exit $status)"
  cat "$work/$name.out" "$work/$name.err"
}

mkdir vault
cd vault

run init 'kbx init without npm' init
[[ $status == 0 ]] || fail "kbx init exited $status, not 0"

run builtin 'kbx doctor without npm, the default' doctor
[[ $status == 0 ]] || fail "doctor on the default exited $status, not 0"
grep -Eq '^search +ok +builtin' "$work/builtin.out" || fail "doctor printed no built-in search row"
if grep -Eq '^qmd ' "$work/builtin.out"; then fail "doctor on the default printed a qmd row"; fi

sed -i.bak 's/^search = .*/search = "qmd"/' .kbx/kbx.toml

run qmd 'kbx doctor without npm, search = "qmd"' doctor
[[ $status == 1 ]] || fail "doctor exited $status, not 1"
grep -Eq '^qmd +fail +not installed' "$work/qmd.out" || fail "doctor printed no failed qmd row"
grep -qF 'Node >= 22' "$work/qmd.err" || fail "doctor's error does not name Node >= 22"
