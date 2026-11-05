# The built-in search engine

## The comparison and its tolerance

Written before any built-in result was seen, so the bar can't drift to fit them.

`qmd_knn` runs through the same generator on the same vault with each backend,
`kbx suggest --judge none --generators qmd_knn`, with the budget lifted. The
built-in engine matches QMD if it passes all three.

| Vault | Measure | Passes if |
|---|---|---|
| Fixture, `tests/fixtures/vault`, labels in `tests/fixtures/bakeoff/synthetic-labels.jsonl` | Labelled pairs found | It misses at most 1 accepted pair QMD finds, and finds at most 1 rejected pair more |
| Synthetic 10k, `tools/gen-vault.py` | Pairs in the same topic, by `tags`, over pairs found | At most 2 points below QMD, with at least 90% of QMD's pairs |
| Bake-off mirror of `~/HQ` | Share of pairs a blind local judge (`qwen3:8b`) calls related, 100 pairs from each backend, shuffled together | At most 5 points below QMD |

A person's labels would beat a local model's on the mirror, but
`tests/fixtures/bakeoff` has none yet and its 100 pairs all come from
`alias_cooccur`, so they say nothing about `qmd_knn`.
