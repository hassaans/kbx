# Judge bake-off

Which model should each backend use by default? §14 of the spec leaves that to a
measurement: the same labelled pairs through every judge, scored by agreement
with a person.

**Status: labels pending.** `tests/fixtures/bakeoff/pairs.jsonl` holds 100
unlabelled pairs from `~/HQ`. There are no numbers yet, and the model defaults
in `kbx.toml` stay as they are until there are.

## Results

<!-- bakeoff:begin -->
No results yet: the pairs are waiting for labels.
<!-- bakeoff:end -->

- **Agreement**: pairs where the judge's accept or reject matched the label, over
  all labelled pairs. `unsure`, an error or no answer counts against it.
- **Accept**, **Reject**: the same, over the pairs a person accepted, or rejected.
- **Relation**: pairs a person accepted with a relation, where the judge accepted
  and named that relation. An inverse counts as its relation.
- **Unsure**: `unsure` verdicts over all labelled pairs.
- **Cost**: what its calls cost over every run, retries and failed ones included.
  Login probes are left out. It's an estimate for `codex-cli`, and 0 for local backends.
- **Wall time**: the time spent in its calls, added up over runs. Local figures
  depend on the machine.

The pairs are chosen for coverage, so rare kinds of pair count for as much as
common ones. These are comparisons between judges, not estimates of how a judge
does on a typical vault.

## What the owner has to do

Everything below happens in a mirror outside `~/HQ`, so labelling can't write to
the vault. Run it from a checkout of this repo.

**1. Build the mirror, if `~/Library/Caches/kbx-bakeoff/hq` is gone.**

```sh
REPO=$PWD
MIRROR=~/Library/Caches/kbx-bakeoff/hq
tools/bakeoff/mirror-hq.py ~/HQ "$MIRROR"
cd "$MIRROR"
kbx init --ontology "$REPO/tools/bakeoff/hq-ontology.yaml"
kbx index
```

**2. Put the pairs in the review queue, and label them.**

```sh
kbx bakeoff --enqueue "$REPO/tests/fixtures/bakeoff/pairs.jsonl"
kbx review
```

Press `a` to accept, choosing a relation, or `r` to reject. Don't run `kbx
suggest` first: the queue should show no judge's opinion. Use `d` or `s` for a
pair you can't decide, and it won't be scored. `--enqueue` warns if a pair's
notes have gone from the mirror.

**3. Export the labels into the repo.**

```sh
kbx export reviews --jsonl > "$REPO/tests/fixtures/bakeoff/labels.jsonl"
```

Check the file before committing it: `reason` is free text, so an explanation
that quotes a note would be published. This repo is public. Instead of steps 2
and 3 you can copy `pairs.jsonl` to `labels.jsonl` and fill in `decision`
(`accept` or `reject`) and `rel`, but the excerpts there are stand-ins, so you'd
need the notes open beside it.

**4. Decide what may leave the machine.** The next step sends excerpts and titles
of these notes, which include companies and people, to the Anthropic and OpenAI
CLIs. §14 asks for plan limits and data-retention terms to be confirmed first.
To keep the run local, pass `--models ollama:qwen3:14b,ollama:qwen3:8b`. `ollama
pull` both models first.

**5. Run the bake-off.**

```sh
kbx bakeoff \
  --labels "$REPO/tests/fixtures/bakeoff/labels.jsonl" \
  --doc "$REPO/docs/judge-bakeoff.md" \
  --budget-usd 5
```

It prints the tables and writes them above. Whatever can't run is named in the
Notes column, and the others carry on. Run it again once that's fixed: pairs
already judged cost nothing.

**6. Commit `labels.jsonl` and this file.** The manager picks the defaults from
the numbers, and that change closes §14.

## `kbx bakeoff`

```
kbx bakeoff --prepare [--pairs 100] [--out FILE] [--synthetic] [--generators a,b]
kbx bakeoff --enqueue FILE
kbx bakeoff [--labels FILE] [--doc FILE] [--models LIST] [--batch N] [--budget-usd X]
```

**`--prepare`** runs the generators and prints JSON Lines. It changes nothing, and
stores no candidates.

- Pairs are taken in turn from each stratum: the generators that found the pair,
  a third of the score range, and the unordered type pair. A note is in at most
  three pairs, unless that leaves too few.
- A pair pinned away from any judge by `[judge.overrides]` is left out, because
  they must all see the same pairs.
- The same vault gives the same pairs.
- A row is a `reviews` row with `rel` and `decision` null, then `cid`,
  `generators`, `score`, `src_type`, `dst_type`, `src_excerpt` and `dst_excerpt`.
  An excerpt is the note's first 20 words. `--synthetic` replaces it with a
  stand-in that quotes nothing, for a file that gets published. `alias_cooccur`'s
  matched term is left out, as it would quote a note.

**`--enqueue`** stores a file's pairs as candidates, so that `kbx review` shows
them. No judge has seen them.

**Without either**, each entry of `--models` judges the labelled pairs, with the
usual batching, retry and cache. `--models` takes `backend` for the model it would
normally use, `KBX_CLAUDE_MODEL` and the like included, or `backend:model` for
exactly that model, which skips the switch to `qwen3:8b` on low memory.
`llama-server` takes no model, as it runs what it was started with. The default is `claude-cli`, `claude-cli:sonnet`, `codex-cli`, `ollama:qwen3:14b` and
`ollama:qwen3:8b`.

- Labels come from `kbx export reviews --jsonl` or a filled-in `--prepare` file.
  A pair with several rows goes by the latest, and on a tie by the accept. If
  that's a deferral, or there's no decision, the pair isn't scored.
- Verdicts are kept in `.kbx/bakeoff.sqlite`, not the graph, so `kbx review` never
  shows them. The cache key is the usual one, so changing the prompt or the
  ontology asks every pair again.
- `--budget-usd` is one limit across all the models.
- `--doc` has to hold `<!-- bakeoff:begin -->` and `<!-- bakeoff:end -->`. It's
  left alone if no judge answers.

Judges are told less than `kbx suggest` tells them, so that nothing gives the
labels away:

- `why_proposed` is empty. Accepting a pair deletes its candidate, so only
  rejected pairs would still have provenance.
- Pairs go out in cid order, lower id first. In a labels file, a pair's place and
  direction can depend on what was decided.

## The pairs in `tests/fixtures/bakeoff/`

| File | What it is |
|---|---|
| `pairs.jsonl` | 100 unlabelled pairs from `~/HQ`. |
| `labels.jsonl` | The labels, once they exist. |
| `synthetic-labels.jsonl` | Made-up labels on the fixture vault, for the tests. |

`~/HQ` as it stands can't be indexed: its pages have `canonical_name` where kbx
wants `title`. So `pairs.jsonl` comes from `tools/bakeoff/mirror-hq.py`, which
writes new notes outside the vault:

```sh
kbx bakeoff --prepare --synthetic \
  --generators schema_gap,alias_cooccur,common_neighbors,adamic_adar,two_hop_bridge \
  --out "$REPO/tests/fixtures/bakeoff/pairs.jsonl"
```

- **Left out of the mirror:** every key but `id`, `type`, `canonical_name` and
  `aliases`, and the `invoice`, `ar-invoice`, `ap-bill`, `journal-entry` and
  `account` pages. 18,159 notes remain, one per id. One more fails to parse.
- **Published:** the 64-digit ids, the types, the generators and the score. The
  excerpts are synthetic, because real ones would name clients and people. The
  ids aren't SHA-256 digests of the titles.
- **Limits of the set:** all 100 pairs come from `alias_cooccur`. The vault has no
  wikilinks, so the structural generators have nothing to work on, and `qmd_knn`
  needs an embedded index of the mirror, which wasn't built. Covering it would
  take a second set from an embedded mirror. Bodies are short, with a
  median under 15 words, and 14 of the 200 note ends have no body at all, so
  judges and labels both lean on titles.
