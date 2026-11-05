#!/usr/bin/env bash
# Installs a published kbx the way a user would and fails unless it works. Run it
# on a machine with Node and without kbx or qmd.
set -euo pipefail

usage="usage: smoke-npm.sh <version> <platform package> <vault>"
version=${1:?$usage}
package=${2:?$usage}
vault=${3:?$usage}
vault=$(cd "$vault" && pwd)

fail() {
  echo "smoke-npm: $*" >&2
  exit 1
}

# Node on Windows prints native paths and reads them from the environment.
to_bash() {
  if command -v cygpath >/dev/null; then cygpath -u "$1"; else echo "$1"; fi
}
to_native() {
  if command -v cygpath >/dev/null; then cygpath -w "$1"; else echo "$1"; fi
}

for tool in kbx qmd; do
  if found=$(command -v "$tool"); then
    fail "$tool is already on this machine, at $found"
  fi
done

npm i -g "kbx@$version"

root=$(to_bash "$(npm root -g)")
shopt -s nullglob
platforms=("$root"/kbx-* "$root"/kbx/node_modules/kbx-*)
shopt -u nullglob
if [[ ${#platforms[@]} -ne 1 ]]; then
  fail "expected one platform package, found ${#platforms[@]}: ${platforms[*]:-none}"
fi
if [[ $(basename "${platforms[0]}") != "$package" ]]; then
  fail "expected $package, found ${platforms[0]}"
fi
echo "platform package: ${platforms[0]}"

printed=$(kbx --version)
[[ $printed == "kbx $version" ]] || fail "kbx --version printed $printed, not kbx $version"
echo "$printed"

home=$(mktemp -d)
HOME=$home
USERPROFILE=$(to_native "$home")
export HOME USERPROFILE
cd "$home"

# Runs kbx with nothing on stdin, which a prompt can't get past, and leaves what
# it printed in $home/<name>.out and .err and its exit code in $status.
run() {
  local name=$1 title=$2
  shift 2
  status=0
  kbx "$@" </dev/null >"$home/$name.out" 2>"$home/$name.err" || status=$?
  echo "--- $title (exit $status)"
  cat "$home/$name.out" "$home/$name.err"
}

qmd_row() {
  sed -n 's/^qmd  *ok  *//p' "$home/$1.out"
}

# Before `kbx init`, which would install qmd itself.
run doctor-1 'kbx doctor, first run' doctor
[[ $status == 0 ]] || fail "the first doctor exited $status"
[[ ! -s $home/doctor-1.err ]] || fail "the first doctor wrote to stderr"
installed=$(qmd_row doctor-1)
[[ -n $installed ]] || fail "the first doctor did not report qmd ok"
command -v qmd >/dev/null || fail "qmd is not on PATH after the first doctor"
npm ls -g @tobilu/qmd || fail "@tobilu/qmd is not in the global packages after the first doctor"

run doctor-2 'kbx doctor, second run' doctor
[[ $status == 0 ]] || fail "the second doctor exited $status"
[[ ! -s $home/doctor-2.err ]] || fail "the second doctor wrote to stderr"
present=$(qmd_row doctor-2)
[[ $present == "$installed" ]] || fail "the second doctor found ${present:-no qmd}, not $installed"

cp -R "$vault" "$home/vault"
cd "$home/vault"

run init 'kbx init' init
[[ $status == 0 ]] || fail "kbx init exited $status"
[[ -f kbx.toml ]] || fail "kbx init made no kbx.toml"

# Each top-level folder is a scope. Init only warns when qmd refuses a collection.
scopes=0
for folder in "$vault"/*/; do
  scope=$(basename "$folder")
  scopes=$((scopes + 1))
  grep -qxF "created qmd collection $scope" "$home/init.out" ||
    fail "kbx init created no qmd collection for $scope"
done

run index 'kbx index' index
[[ $status == 0 ]] || fail "kbx index exited $status"
expected='indexed 25, 0 unchanged, removed 0, 2 skipped, 2 failed, 2 hidden links'
[[ $(cat "$home/index.out") == "$expected" ]] || fail "kbx index printed $(cat "$home/index.out"), not $expected"

# The fixture holds one of every finding on purpose, so lint has to fail on it.
run lint 'kbx lint' lint
[[ $status == 1 ]] || fail "kbx lint exited $status, not 1"
summary=$(tail -n 1 "$home/lint.out")
[[ $summary == '12 errors, 1 warning' ]] || fail "kbx lint printed $summary, not 12 errors, 1 warning"

run doctor-3 'kbx doctor, in the vault' doctor
[[ $status == 0 ]] || fail "doctor in the vault exited $status"
grep -Eq '^vault +ok ' "$home/doctor-3.out" || fail "doctor in the vault did not find it"
grep -Eq '^index +ok +25 notes' "$home/doctor-3.out" || fail "doctor in the vault did not report the index"
grep -Eq "^collections +ok +$scopes in index " "$home/doctor-3.out" ||
  fail "doctor in the vault did not report $scopes collections"
