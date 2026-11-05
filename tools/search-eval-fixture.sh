#!/bin/sh
# Scores qmd_knn on the fixture vault, against its made-up labels, with one
# backend. See docs/search-engine.md.
#
#   tools/search-eval-fixture.sh <kbx binary> <empty folder> <qmd|builtin>
set -eu

kbx=$1
vault=$2
backend=$3
repo=$(cd "$(dirname "$0")/.." && pwd)

cp -R "$repo/tests/fixtures/vault/." "$vault"
cd "$vault"
"$kbx" init > /dev/null
sed -i.bak "s/^search = .*/search = \"$backend\"/" kbx.toml && rm kbx.toml.bak
"$kbx" index > /dev/null 2>&1 || true
python3 - "$repo/tests/fixtures/bakeoff/synthetic-labels.jsonl" <<'PY'
import json, sqlite3, sys

graph = sqlite3.connect(".kbx/graph.sqlite")
for line in open(sys.argv[1]):
    row = json.loads(line)
    graph.execute(
        "INSERT INTO reviews (src, dst, rel, decision, reason, strikes, decided_at)"
        " VALUES (?, ?, ?, ?, ?, ?, ?)",
        [row[k] for k in ("src", "dst", "rel", "decision", "reason", "strikes", "decided_at")],
    )
graph.commit()
PY
# A real run first, so that each backend's index is up to date.
"$kbx" suggest --judge none --generators qmd_knn > /dev/null
"$kbx" suggest --eval --generators qmd_knn
