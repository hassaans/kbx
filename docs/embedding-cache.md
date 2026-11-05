# Embedding cache

This page settles the §14 open question on vectors: is repeated `qmd_knn` work a
real cost, and if so, what should kbx cache? It also holds the baseline for the
built-in search engine task, so everything here can be run again.

**The answer:** the cost is real, but it isn't embedding. A second `kbx suggest` on
an unchanged 10,000-note vault took as long as the first, 33 minutes, and 99% of
that was `qmd query`. Embedding the notes again costs nothing, because qmd already
skips them. So kbx caches search results, not embeddings, and the same second run
now takes 9 seconds.

## What was measured

All runs are `kbx suggest --judge none` with the real qmd 1.0.7. The "before" runs
use main at ac9d290. Each run is one sample, on a machine that was also running
other sessions' test suites (load average 2 to 4 on 10 cores).

### Before any change

| | First run | Second run, unchanged | After editing 10 notes |
|---|---|---|---|
| Wall time | 1,948.8 s | 1,964.8 s | 1,969.2 s |
| CPU time, kbx and qmd | 556.3 s | 559.1 s | 571.1 s |
| qmd processes | 102 | 102 | 102 |
| `qmd query` calls | 100 | 100 | 100 |
| `qmd query` wall | 1,936.7 s | 1,951.6 s | 1,956.1 s |
| `qmd query` CPU | 545.9 s | 548.7 s | 560.3 s |
| `qmd update` wall | 1.5 s | 2.8 s | 1.6 s |
| `qmd embed` wall | 0.5 s | 0.5 s | 1.6 s |
| All qmd wall | 1,938.7 s | 1,954.9 s | 1,959.3 s |
| All qmd CPU | 547.4 s | 550.2 s | 562.3 s |
| qmd's share of wall time | 99.5% | 99.5% | 99.5% |
| kbx's own wall (total less qmd) | 10.1 s | 9.9 s | 9.9 s |

- **Most of the wall time isn't CPU.** A query uses about 5.5 s of CPU and takes
  about 19 s (median 18 s, range 8 to 32 s). The rest is presumably model loading
  and GPU work, which `time` doesn't count. Every query is a new process, so each
  one loads its models again.
- **kbx alone** takes 12.6 s wall and 6.0 s CPU on a fake qmd that answers every
  call at once with no hits (`fake` in the script below). That's the same size as
  the 10 s the table leaves over.
- **The first embed, once:** 12,322 chunks from 10,000 notes, 532.9 s wall (qmd
  reports 8m 41s), 88.7 s CPU, 1.5 GiB peak. The qmd index came to 131 MB.

### Why the second run wasn't faster

qmd has its own cache of query expansions and rerank scores, the `llm_cache` table.
It works: one query repeated straight away drops from 15.2 s to 1.65 s. But qmd
trims the table to 1,000 rows, and 100 note-length searches need several thousand,
each rerank score being a row of its own. It held 1,050 rows after the first run
and 1,257 after the second, and the second run saved nothing. That the trim is the
cause comes from reading qmd's `rerank` and `setCachedResult`, not from a test.

### After the cache

Same protocol, from the same snapshot of the vault and the qmd index:

| | First run | Second run, unchanged | Third, unchanged | After editing 10 notes |
|---|---|---|---|---|
| Wall time | 1,914.1 s | **8.9 s** | 7.3 s | **41.5 s** |
| CPU time, kbx and qmd | 531.8 s | 6.1 s | 5.9 s | 18.2 s |
| qmd processes | 104 | 1 | 1 | 5 |
| `qmd query` calls | 100 | 0 | 0 | 1 |
| `qmd update` / `embed` calls | 1 / 1 | 0 / 0 | 0 / 0 | 1 / 1 |
| `qmd status` calls | 2 | 1 | 1 | 2 |
| All qmd wall | 1,903.6 s | 1.1 s | 1.0 s | 35.0 s |

The first run is no faster: nothing has been searched yet. The binary measured here
differs from the final one in one path, a run whose refresh fails, which these runs
don't reach.

The cache file was 434 KB for 100 rows.

## The decision

The test was whether a second run repeats more than about 20% of the first run's
time as qmd work. It repeated 100%, so the cache is implemented.

| Option | Verdict |
|---|---|
| Cache embeddings of notes | No. `qmd embed` takes 0.5 s on an unchanged vault and already skips what it has. There's nothing to save, and kbx couldn't hand vectors to `qmd query` anyway. |
| Read qmd's `index.sqlite` through a sidecar | No. It would mean running kbx's own similarity search over a schema that belongs to qmd, and the schema isn't shown to be stable from 1.0.7 to 2.x. The built-in engine is the better home for that work. |
| Cache search results in `.kbx/` | Yes. It removes the whole repeated cost and needs nothing from qmd but its command line. |

## How the cache works

`CachedSearch` in `crates/kbx-gen/src/cache.rs` wraps `QmdSearch` behind the same
`Search` trait, so `qmd_knn` doesn't change. It keeps one row per scope and note in
`.kbx/search-cache.sqlite`, and only for the QMD backend.

A note's search is reused while all of this holds:

- The text kbx sends is the same. That text comes from the note's sections, so this
  is the sections' content, as far as the search can see it.
- qmd reports the same models, from the `Models` lines of `qmd status`, which are
  the same in 1.0.7 and 2.8.3.
- The qmd index, the collections and the hit limit are the same.
- Every section it hit still has the `text_hash` it had. Comparing sections rather
  than files means a frontmatter edit, such as `kbx suggest --stage` or `kbx accept`
  writing a relation, repeats no search.

`qmd update` and `qmd embed` are skipped when every note's path and content hash are
what they were at the last successful refresh and `qmd status` reports the same file
and vector counts. Reporting the counts is how a lost or emptied qmd index gets
noticed. The `.kbx/` file can be deleted at any time; the next run searches again.

Some cases don't use the cache:

- A qmd that doesn't name its models is never cached, because kbx couldn't tell when
  they changed.
- A run whose refresh fails neither reads nor writes the cache, so results from a
  half-built index aren't kept.
- `--dry-run` and `--eval` read the cache but don't write to it.

### Per scope

Rows belong to a scope, and leave with it. But `qmd_knn` searches all of the vault's
collections at once, so a note's hits can be in any scope. The collections searched
are therefore part of each row's key: adding or removing a scope repeats every
search, because the set being searched has changed.

### Limits

- **A row can miss a new neighbour.** If an edit makes a note a neighbour of an
  unchanged note it wasn't near before, that unchanged note's row doesn't know. The
  pair is still found from the edited note's own search, if the run reaches it. Delete
  the file to search everything again.
- **Edits made without `kbx index` aren't seen by the refresh check,** which reads the
  graph. Run `kbx index` first, as usual.
- **A qmd upgrade that keeps its models keeps the rows,** even if it scores differently.
- **The 100-search cap counts cached searches.** A run therefore keeps looking at the
  same 100 least-linked notes, and the other 9,900 are never searched. Not counting
  them would let each run reach 100 more notes, but then a second run would do
  embedding work again, so that's a separate decision.

## kbx and qmd's index

QMD owns the vectors. kbx runs qmd's commands and opens none of its files, and the
tests check that (`kbx_writes_nothing_into_qmds_index_directory`).

qmd writes to its own index when kbx asks it to: `update` and `embed` do, and `query`
fills `llm_cache`. With the real qmd, the index's SHA-256 was the same before and
after each cached run on an unchanged vault (`5d2cf996b0cfbdba…`), because those runs
only call `qmd status`. A run that does search or refresh still changes the file, by
qmd's hand, as it always has.

## For the search engine baseline

- **qmd_knn adds nothing to the kept candidates on this vault.** On its own it found
  1,940 pairs, but none of the 2,000 kept in a full run came from it. The structural
  generators found about 760,000 pairs and fill the budget. So 33 minutes of model
  time bought no candidate. The vault's hubs are synthetic and may overstate this, so
  check it on a real vault before drawing conclusions from it.
- **kbx's own memory** peaks at 2.2 GiB on this vault, with a fake qmd. The 6 GiB in
  the bench output's first lines is the largest qmd process, because `time -l` reports
  the largest child.
- **The cold number to beat:** about 19 s per note searched and 1,940 s per 100
  notes, plus 533 s to embed once.
- Runs vary a little: the over-budget count differed by a few pairs between the two
  unchanged "before" runs, so qmd's results aren't exactly repeatable.

## Run it again

Machine: Apple M2 Pro (6 performance and 4 efficiency cores), 16 GB, macOS 26.3 on AC
power. qmd 1.0.7 (179d450) with `embeddinggemma-300M`, `Qwen3-Reranker-0.6B` and
`qmd-query-expansion-1.7B`, Metal offload on. Node 24.14.0, rustc 1.97.1, Python
3.15.2, release build.

The vault has 10,000 notes in 5 scopes and 25 topics: 102 to 813 body words (median
307), 2 to 5 headings each, up to 3 aliases, relations on most notes and inline
wikilinks on about a fifth. `gen` prints a digest, which should be `9e25328589b0…`
with the arguments below (`602816cc40e0…` after `edit`).

```sh
B=~/Library/Caches/kbx-bench
cargo build --release -p kbx --locked
cp "$CARGO_TARGET_DIR/release/kbx" $B/kbx

tools/gen-vault.py gen $B/vault
cd $B/vault
$B/kbx init
$B/kbx index
# The index is kbx- and the first 6 bytes of the SHA-256 of the vault's real path.
INDEX=kbx-$(printf %s "$(pwd -P)" | shasum -a 256 | cut -c1-12)
qmd --index $INDEX update
/usr/bin/time -l qmd --index $INDEX embed
cd -

tools/bench-suggest.sh $B/kbx $B/vault run1
tools/bench-suggest.sh $B/kbx $B/vault run2
tools/gen-vault.py edit $B/vault --count 10 --round 1
(cd $B/vault && $B/kbx index)
tools/bench-suggest.sh $B/kbx $B/vault run3-edited
tools/bench-suggest.sh $B/kbx $B/vault run4-fake fake
```

`tools/bench-suggest.sh` puts a wrapper called `qmd` first on `PATH`, which runs the
real qmd under `/usr/bin/time -p` and logs each call to `logs/<label>/calls`. kbx's
share is the run's wall time less the sum of those calls, which works because kbx
runs them one at a time. For a binary built after the cache, delete
`.kbx/search-cache.sqlite` to get a cold run. Nothing here belongs in the repo or CI
but the two scripts.
