# Judging and what it costs

`kbx suggest` judges candidates with the first ready backend in `judge.order`.
That sends excerpts of your notes to the logged-in `claude` or `codex` CLI. Use
`--judge none` to stop at the candidates, and `[judge.overrides]` to keep a
folder from a backend. `kbx watch --suggest` never judges.

## Batches

Pairs go out 10 to 25 per call: `--batch`, else `batch` under `[judge]`, else
20. Each batch's verdicts are written as it finishes, so a run that stops keeps
what it paid for.

`--dry-run` calls nothing and prints the batches a real run would send, with
their token estimates, leaving out pairs already judged and pairs kept away by
`judge.overrides`. Nothing is probed, so under `auto` it assumes the first
backend in `judge.order`.

## The verdict cache

A pair is never judged twice under the same `(cid, backend, model,
prompt_hash)`. `prompt_hash` covers `prompts/judge.system.md`,
`schema/judge.schema.json` and the ontology file, so changing any of them asks
every pair again. Changing a note does not: `cid` depends only on the two ids.

Going back to a judge costs nothing and makes its cached verdicts the latest, so
`kbx review` shows that judge's answers again.

The prompt and schema are compiled into the binary, so a change to either takes
effect with the build that carries it.

## Failures

A batch that fails is retried once, in two halves. A half that fails again has
its pairs marked `error` in `verdicts`, with the reason as the `rationale`, and
the run moves on. Error rows aren't verdicts: they stay out of `kbx review`, and
the next run asks those pairs again. A lost login stops the run with no retry.

A failed call that still cost money counts against the budget. Its cost reaches
`verdicts` only if it was the call that marked the rows, so after a retry
`SUM(cost_usd)` can be less than the spend `kbx suggest` printed.

## Budgets

`--budget-usd X` stops before the next call would take this run past `X`, and
prints what was spent. The limit is per run: run again to carry on, and pairs
already judged cost nothing.

The once-a-day login probe is a paid call too. It counts towards the spend and
isn't made if the budget can't cover it. Its cost isn't in `verdicts`.

| Backend | Spend |
|---|---|
| `claude-cli` | The sum of `total_cost_usd` |
| `codex-cli` | An estimate: tokens at the configured rates |
| Local backends | 0 |

A call's cost is only known afterwards, so the check goes by a forecast: the
dearest pair so far this run, times the pairs in the next call. Before any call
has been priced it goes by the backend's rates and an estimate of the batch's
tokens. A call that costs more than forecast can still take the run over `X`,
by at most that difference.

## Rates

Rates are in USD per million tokens. All three are required.

```toml
[judge]
codex = { model = "gpt-6-luna", rates = { input = 0.10, cached_input = 0.01, output = 0.50 } }
```

`kbx init` writes `codex` and `claude` as inline tables, so add `rates` there:
a separate `[judge.codex]` table in the same file is a TOML error.

| Backend | `input` | `cached_input` | `output` | Used for |
|---|---|---|---|---|
| `codex-cli` | 0.10 | 0.01 | 0.50 | All of its spend, and the first forecast |
| `claude-cli` | 0.10 | 0.01 | 0.50 | The first forecast only |

The defaults are for `gpt-6-luna` and Claude Haiku, as of November 2026. The
`gpt-6-luna` figures come from third-party price lists, and Haiku's
`cached_input` is assumed to be a tenth of `input`, so check them against your
provider. Set `rates` when you use another model: the defaults don't follow
`model`. Under a subscription login the figures are notional.

## The queue

`unsure` verdicts go to the end of the queue, unranked. `confidence` is stored
and never used: nothing is accepted or ranked by it. That rule lives with
`UNSURE` in `crates/kbx-core/src/review/queue.rs`.
