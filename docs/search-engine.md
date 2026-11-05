# The built-in search engine

`search = "builtin"` in `kbx.toml` gives `qmd_knn` a search engine inside kbx: BM25
and a local embedding model, with no Node and no qmd. `search = "qmd"` keeps the QMD
path exactly as it was.

**The answer:** on the three vaults below it matches QMD within the tolerance, and a
first run on the 10,000-note vault takes about 10 minutes instead of about 41. The
evidence for quality is thin in one place, which [Limits](#limits-of-this-evidence)
spells out: nobody has labelled a `qmd_knn` pair on a real vault.

## How it works

- **Keywords:** an SQLite FTS5 table with the `porter unicode61` tokenizer. kbx
  already bundles SQLite with FTS5, so this adds no dependency.
- **Vectors:** [`BAAI/bge-small-en-v1.5`](https://huggingface.co/BAAI/bge-small-en-v1.5)
  (MIT) on the CPU with candle, 384 numbers per section. Up to 512 tokens of a section
  are read; the keyword index holds all of it. On macOS candle uses Apple's Accelerate,
  which comes with the system.
- **A note's neighbours:** the mean of its section vectors against every section,
  which needs no model at search time, and BM25 on the note's text, using its 32
  longest words. The two rankings are joined by reciprocal rank fusion. A search reads
  every vector, which takes milliseconds at this size, so there's no approximate index.
- **The index:** `.kbx/search-index.sqlite`, one row per distinct `sections.text_hash`,
  with `PRAGMA user_version` for its format and the model's id and revision beside the
  vectors. A section is embedded when its text is new and never again. Delete the file
  at any time; the next `kbx suggest` rebuilds it. Each batch is committed, so a run
  that's stopped keeps its work.

`kbx suggest --dry-run` and `--eval` read the index without changing it, and with no
index `qmd_knn` is skipped with a message. There's no `kbx search` command yet. The
engine answers free text through `Query::Text` for when there is.

### The model

kbx downloads three files, 134 MB in all, the first time it has something to embed.
They're pinned to a Hugging Face revision and checked against SHA-256 digests.

| OS | Where they go |
|---|---|
| macOS | `~/Library/Caches/kbx/models/` |
| Linux | `$XDG_CACHE_HOME/kbx/models/`, or `~/.cache/kbx/models/` |
| Windows | `%LOCALAPPDATA%\kbx\models\` |

**With no network,** `kbx suggest` searches by keyword alone, says so on stderr, and
tries again next run. Sections embedded later join the vector search then.

**To install the model by hand,** put `config.json`, `tokenizer.json` and
`model.safetensors` from the pinned revision in a folder and set `KBX_MODEL_DIR` to it.
`KBX_MODEL_URL` names a mirror to download from, and the digests still apply.

## The decision

The constraints: all five release targets build with no new system dependency,
`cargo install kbx --locked` keeps working, so no git dependencies, nothing touches the
network at build time, and the model's license allows redistributing its outputs.

| Option | Verdict |
|---|---|
| `qntx/qmd` crate | **No.** crates.io has only 0.3.2 from February 2026; 0.6.0 is on git alone, which breaks `cargo install`. Either way it needs `llama-cpp-sys-2`, which builds llama.cpp with cmake and a C++ compiler: 0.3.2 fails here at that step, on a machine with no cmake. |
| fastembed | **No.** It runs on `ort`, whose build downloads ONNX Runtime, and loading it at run time instead would be a new system dependency. Its defaults also bring in `onig_sys` and native TLS. |
| tract | **Not tried.** Pure Rust, so it should meet the constraints, but candle met them first. Its speed wasn't measured. |
| candle | **Yes.** Pure Rust, reads safetensors, and the release dry run passes on all five targets. |
| tantivy for BM25 | **No.** A second index format and many more crates, for what FTS5 already does. |
| `multilingual-e5-small` (MIT, 471 MB) | **No, on the numbers.** It was 7 points behind QMD on the mirror (14 against 21), outside the tolerance, at 3.5 times the download. |
| EmbeddingGemma, QMD's model | **No.** The Gemma terms carry use restrictions that a plain MIT or Apache license doesn't. |

**A cost of this choice:** `bge-small-en-v1.5` is an English model. On notes in another
language BM25 works as usual but vectors will be weak, where QMD's model is
multilingual. That wasn't measured, since every vault here is English. Pick QMD for
such a vault.

**`CachedSearch` doesn't wrap it.** It exists because a qmd search costs 19 seconds. A
built-in search costs under 0.1 s, so the cache could save about 6 of a warm run's
15 seconds, and it would bring back its known limit, a row that misses a new neighbour.

## The comparison and its tolerance

The table of bars was committed before any built-in result was seen, so that they
couldn't drift to fit. `qmd_knn` runs through the same generator on the same vault with
each backend, `kbx suggest --judge none --generators qmd_knn`, with the budget lifted.
The built-in engine matches QMD if it passes all three.

| Vault | Measure | Passes if |
|---|---|---|
| Fixture, `tests/fixtures/vault`, labels in `tests/fixtures/bakeoff/synthetic-labels.jsonl` | Labelled pairs found | It misses at most 1 accepted pair QMD finds, and finds at most 1 rejected pair more |
| Synthetic 10k, `tools/gen-vault.py` | Pairs in the same topic, by `tags`, over pairs found | At most 2 points below QMD, with at least 90% of QMD's pairs |
| Bake-off mirror of `~/HQ` | Share of pairs a blind local judge (`qwen3:8b`) calls related, 100 pairs from each backend, shuffled together | At most 5 points below QMD |

A person's labels would beat a local model's on the mirror, but `tests/fixtures/bakeoff`
has none yet and its 100 pairs all come from `alias_cooccur`, so they say nothing about
`qmd_knn`.

### Results

| Vault | QMD | Built-in | Verdict |
|---|---|---|---|
| Fixture | 3 of 3 accepted, 3 of 3 rejected found | 3 of 3 accepted, 2 of 3 rejected found | Pass |
| Synthetic 10k | 1,917 of 1,941 pairs in topic, 98.8% | 1,997 of 1,997, 100% | Pass |
| Mirror, 100 pairs each | 20 related | 17 related | Pass, 3 points below |
| Mirror, 400 pairs each, added afterwards | 73 related, 18.2% | 68 related, 17.0% | 1.2 points below |

### Limits of this evidence

- **The mirror's margin is within its noise.** At 100 pairs a side the standard error is
  about 4 points, so "3 points below" can't tell a small loss from none. The 400-pair run
  was added for that reason, and pairs from one note aren't independent, so even that
  overstates the precision.
- **The judge is a small model,** the same for both sides, and calls only about a fifth
  of either backend's pairs related. It ranks; it doesn't say how good either one is.
- **The engines find different pairs.** Only 11% of pairs overlap on the mirror (387 in
  common of 1,823 and 1,984) and 7% on the 10k vault. Matching on the rate doesn't mean
  finding the same links.
- **The 10k measure is nearly saturated:** notes in a topic share its terms, which BM25
  alone finds.
- **The fixture has 6 labels,** and they were made up for tests.
- **A late fix changed the query.** An earlier build chose BM25 words alphabetically and
  scored 18 and 69 on the mirror. Every built-in number here is from the final code.
- **One engine setting, tuned on nothing:** the fusion constant and pool size are the
  first values tried. The model was picked on these same vaults, from two candidates.

## Does `qmd_knn` matter?

`docs/embedding-cache.md` found that on the synthetic vault none of the 2,000 kept
candidates came from `qmd_knn`, and asked whether that was an artefact of synthetic hubs.
It isn't only that. On the mirror, a full `kbx suggest` with real qmd kept 2,000
candidates, all from `alias_cooccur`, with 172,566 pairs over budget. `qmd_knn` also
found 34 of those pairs, and none of its own. The mirror has no links at all, so it's
the opposite vault to the synthetic one, and the budget still fills without `qmd_knn`.

So with the default budget the search backend changed nothing of what was kept on either
vault. It matters under `--generators qmd_knn`, and on a vault whose other generators
don't fill the budget. For the default, the gains in time and in needing no Node are the
larger ones. Whether `qmd_knn` deserves a share of the budget is a separate question.

## Timings

The 10k vault of `docs/embedding-cache.md`, `kbx suggest --judge none`, one sample each,
on the same M2 Pro with little else running. QMD's figures are from that page; the
built-in runs had 20 to 30 notes edited since, which changes nothing here.

| | QMD | Built-in |
|---|---|---|
| First run, including the one embed | 2,481 s (533 + 1,948) | 627 s |
| Second run, unchanged | 8.9 s | 14.8 s |
| After editing 10 notes | 41.5 s | 20.7 s |
| Peak memory, first run | 6 GiB in qmd, 2.2 GiB in kbx | 3.2 GiB |
| Index on disk | 131 MB | 140 MB |

- **The second run is slower than QMD's cached one,** by about 6 seconds, because it
  really searches.
- **Peak memory** was 3.8 GiB on the mirror, where a few long sections fill a batch.
- **Linux and Windows have no Accelerate.** Candle's own matrix code reached 3,730
  tokens/s on this machine against 6,895 with it, so expect a first run about twice as
  long. They weren't timed on their own hardware.
- A cold mirror, 7,097 distinct sections, took 111 s on a busy machine.

## Run it again

The vaults and qmd indexes are as in `docs/embedding-cache.md`. Run qmd jobs one at a
time: two at once, or qmd next to Ollama, can abort in `ggml_metal_synchronize`, and one
failed search makes `kbx suggest` skip `qmd_knn` altogether. Finished searches stay in
the search cache, so running again carries on.

```sh
# Fixture, with each backend, in an empty folder each.
tools/search-eval-fixture.sh "$PWD/target/release/kbx" /tmp/fixture-qmd qmd
tools/search-eval-fixture.sh "$PWD/target/release/kbx" /tmp/fixture-builtin builtin

# In a copy of a vault per backend, with `candidates` empty:
kbx suggest --judge none --generators qmd_knn --budget 100000

tools/compare-search.py overlap <qmd graph> <builtin graph>
tools/compare-search.py topics <graph> <vault>
ollama pull qwen3:8b
tools/compare-search.py grade <qmd graph> <qmd vault> <builtin graph> <builtin vault>
```

Copy the mirror rather than running in it: it holds the bake-off's labels. `grade` sends
notes only to the local Ollama.

The tests use `tests/fixtures/tiny-model`, a BERT of random weights from
`tools/make-tiny-model.py`, and a fake embedder behind the `Embedder` trait, so CI
downloads no model. What the tests can't show is what's on this page.
