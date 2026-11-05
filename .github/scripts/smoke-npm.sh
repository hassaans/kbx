#!/usr/bin/env bash
# Installs a published kbx the way a user would and fails unless it works: first
# the default, which must never touch qmd, then the QMD option. `kbx suggest`
# downloads the real model. Run it on a machine with Node and without kbx or qmd.
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

npm i -g "kbx-cli@$version"

root=$(to_bash "$(npm root -g)")
shopt -s nullglob
platforms=()
# The shim's own folder matches kbx-* too.
for folder in "$root"/kbx-* "$root"/kbx-cli/node_modules/kbx-*; do
  [[ $(basename "$folder") == kbx-cli ]] || platforms+=("$folder")
done
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

no_qmd() {
  if found=$(command -v qmd); then fail "$1 installed qmd, at $found"; fi
  if npm ls -g @tobilu/qmd >/dev/null 2>&1; then fail "$1 installed @tobilu/qmd"; fi
}

run doctor-1 'kbx doctor, outside a vault' doctor
[[ $status == 0 ]] || fail "doctor outside a vault exited $status"
[[ ! -s $home/doctor-1.err ]] || fail "doctor outside a vault wrote to stderr"
no_qmd 'doctor outside a vault'

cp -R "$vault" "$home/vault"
cd "$home/vault"

run init 'kbx init' init
[[ $status == 0 ]] || fail "kbx init exited $status"
grep -qx 'search = "builtin".*' kbx.toml || fail "kbx init did not choose the built-in engine"
if grep -q 'qmd collection' "$home/init.out"; then fail "kbx init set up qmd by default"; fi
no_qmd 'kbx init'

run index 'kbx index' index
[[ $status == 0 ]] || fail "kbx index exited $status"
expected='indexed 25, 0 unchanged, removed 0, 1 skipped, 2 failed, 2 hidden links, 0 inferred'
[[ $(head -n 1 "$home/index.out") == "$expected" ]] || fail "kbx index printed $(head -n 1 "$home/index.out"), not $expected"

# The fixture holds one of every finding on purpose, so lint has to fail on it.
run lint 'kbx lint' lint
[[ $status == 1 ]] || fail "kbx lint exited $status, not 1"
summary=$(tail -n 1 "$home/lint.out")
[[ $summary == '12 errors, 1 warning' ]] || fail "kbx lint printed $summary, not 12 errors, 1 warning"

run suggest 'kbx suggest, with the built-in engine' suggest --judge none --generators qmd_knn
[[ $status == 0 ]] || fail "kbx suggest exited $status"
grep -Eq '^ +qmd_knn +[1-9]' "$home/suggest.out" || fail "kbx suggest found no qmd_knn candidates"
no_qmd 'kbx suggest'

run doctor-2 'kbx doctor, in the vault' doctor
[[ $status == 0 ]] || fail "doctor in the vault exited $status"
grep -Eq '^vault +ok ' "$home/doctor-2.out" || fail "doctor in the vault did not find it"
grep -Eq '^index +ok +25 notes' "$home/doctor-2.out" || fail "doctor in the vault did not report the index"
grep -Eq '^search +ok +builtin, model ready' "$home/doctor-2.out" ||
  fail "doctor in the vault did not report the built-in engine"
no_qmd 'doctor in the vault'

# The QMD option. Doctor installs qmd for a vault that asks for it, silently.
cp -R "$home/vault" "$home/vault-qmd"
cd "$home/vault-qmd"
sed -i.bak 's/^search = .*/search = "qmd"/' kbx.toml
rm kbx.toml.bak

run doctor-3 'kbx doctor, search = "qmd", first run' doctor
[[ $status == 0 ]] || fail "the first qmd doctor exited $status"
[[ ! -s $home/doctor-3.err ]] || fail "the first qmd doctor wrote to stderr"
installed=$(qmd_row doctor-3)
[[ -n $installed ]] || fail "the first qmd doctor did not report qmd ok"
command -v qmd >/dev/null || fail "qmd is not on PATH after the first qmd doctor"
npm ls -g @tobilu/qmd || fail "@tobilu/qmd is not in the global packages after the first qmd doctor"

run doctor-4 'kbx doctor, search = "qmd", second run' doctor
[[ $status == 0 ]] || fail "the second qmd doctor exited $status"
[[ ! -s $home/doctor-4.err ]] || fail "the second qmd doctor wrote to stderr"
present=$(qmd_row doctor-4)
[[ $present == "$installed" ]] || fail "the second qmd doctor found ${present:-no qmd}, not $installed"

cp -R "$vault" "$home/vault-init"
cd "$home/vault-init"

run init-qmd 'kbx init --search qmd' init --search qmd
[[ $status == 0 ]] || fail "kbx init --search qmd exited $status"
grep -qx 'search = "qmd".*' kbx.toml || fail "kbx init --search qmd did not choose qmd"

# Each top-level folder is a scope. Init only warns when qmd refuses a collection.
scopes=0
for folder in "$vault"/*/; do
  scope=$(basename "$folder")
  scopes=$((scopes + 1))
  grep -qxF "created qmd collection $scope" "$home/init-qmd.out" ||
    fail "kbx init --search qmd created no qmd collection for $scope"
done

run doctor-5 'kbx doctor, after kbx init --search qmd' doctor
[[ $status == 0 ]] || fail "doctor on the qmd vault exited $status"
grep -Eq '^search +ok +qmd' "$home/doctor-5.out" || fail "doctor on the qmd vault did not report qmd"
grep -Eq "^collections +ok +$scopes in index " "$home/doctor-5.out" ||
  fail "doctor on the qmd vault did not report $scopes collections"
