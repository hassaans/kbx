# Generators: structure, inline links, the budget and `alias_cooccur`

`common_neighbors`, `adamic_adar` and `two_hop_bridge` read typed relations and
inline `[[links]]`. An inline link counts for less, and a typed pair scores
exactly as it would without any inline links. On a vault with no inline links,
nothing changes.

**The answer:** `tests/fixtures/existing-vault`, which has inline links alone, goes
from no structural evidence to 4 `common_neighbors` and `adamic_adar` pairs and 2
`two_hop_bridge` pairs. On the 10,000-note vault, 70 of the 2,000 kept pairs change,
and every one that came in has a typed neighbour in common. The weight is reasoned,
not tuned: nobody has labelled an inline-only pair yet.

## The weight

In `.kbx/kbx.toml`:

```toml
[generators]
mention_weight = 0.25  # from 0 to 1
```

The key is optional, and a value outside 0 to 1 is an error. **At 0 the
generators read typed edges alone**, which reproduces what they did before they
read inline links, on any vault. An inline link still links its two notes at 0, so
the pair is never proposed.

The default is below `see_also` at 0.3, the weakest relation in the example
ontology. If your ontology has a weaker relation, an inline link outweighs it in
`two_hop_bridge` unless you lower this too.

| Generator | A pair sharing X by typed edges | One end inline | Both ends inline |
|---|---|---|---|
| `common_neighbors` | 1 | `w` | `w²` |
| `adamic_adar` | `1 / ln(typed degree)` | `w / ln(degree)` | `w² / ln(degree)` |
| `two_hop_bridge` | product of the relation weights | relation weight × `w` | `w²` |

`w` is `mention_weight`. `degree` counts X's typed and inline neighbours. Each is
summed over every X, so it takes 16 notes shared inline to tie with one shared by
typed edges in `common_neighbors`. Typed rank is a strong preference, then, and not
an absolute rule.

The candidate's shape doesn't change: the same generator names, the same `via`, and
the same cid, since a cid doesn't depend on which generator found a pair.

## What counts as a neighbour

- **Both directions count in `common_neighbors` and `adamic_adar`.** Typed edges
  have mirror rows and inline links don't, so `A→X` makes A and X neighbours either
  way, and two notes that one note links to share it.
- **`two_hop_bridge` keeps its direction.** Only `A→X←B` is a bridge, so two notes
  that X links to aren't one.
- **A pair linked inline is not proposed,** the same as one with a typed edge.
- **A typed edge wins** where it joins the same two notes as an inline link. In
  `two_hop_bridge` the heavier of the two counts, which at the default is the typed
  edge for every relation in the example ontology.
- **A relation accepted in `.kbx/` is a typed edge,** and counts exactly as one in
  the frontmatter does.
- **`qmd_knn` is unchanged.** "Least linked first" still means typed degree, so the
  notes it searches stay the same.

## Hubs

Typed hubs work as §5 says: damped in `common_neighbors` and `adamic_adar`, and no
bridge except by `part_of`. Inline links are stricter. **All the inline links of a
hub are ignored,** in every generator and at both ends of a pair. A hub here is a
note that's a typed hub, or has more typed and inline neighbours than the vault's
p99 or than 200, whichever is lower.

Damping wasn't enough, and the stress row below is why. One index note linking to
5,000 notes means 12.5 million pairs through it, and a run with damping was still
going after 9 minutes, against 5 seconds without. Ignoring only its role as the
shared note wasn't enough either. As an end of a pair, that index note scored 9.0
and pushed out a typed pair. The 200 is there because p99 is relative: a vault
with many long lists would have a high p99.

The cost in a small vault is that the note with the most links is usually above p99,
so it gets ignored too. In `existing-vault` that's the Health Check.

## Measurements

Before is `a4d7c8c`, after is this change at the default weight, each on a copy of
the vault with an empty `candidates` table. "Proposed" is `kbx suggest --eval`, with
no budget. "Kept" is the `kbx suggest --judge none` summary.

| Vault | Inline links | Generator | Proposed before | Proposed after | Kept before | Kept after |
|---|---|---|---|---|---|---|
| `existing-vault` | 10 | `common_neighbors` | 0 | 4 | 0 | 4 |
| | | `adamic_adar` | 0 | 4 | 0 | 4 |
| | | `two_hop_bridge` | 0 | 2 | 0 | 2 |
| | | all (11 pairs either way) | | | 11 | 11 |
| `vault` | 1 | `common_neighbors` | 84 | 84 | 75 | 75 |
| | | `adamic_adar` | 84 | 84 | 75 | 75 |
| | | `two_hop_bridge` | 18 | 18 | 18 | 18 |
| | | all | | | 229 | 229 |
| 10,000 notes | 3,124 | `common_neighbors` | 301,090 | 314,891 | 2,000 | 2,000 |
| | | `adamic_adar` | 301,090 | 314,891 | 2,000 | 2,000 |
| | | `two_hop_bridge` | 264,991 | 268,558 | 1,504 | 1,524 |
| | | `schema_gap` | 473,432 | 473,432 | 99 | 104 |
| | | all | | | 2,000 | 2,000 |
| 10,000 + an index note of 5,000 links | 8,124 | `common_neighbors` | 301,090 | 314,891 | 1,972 | 1,972 |
| | | `two_hop_bridge` | 264,991 | 268,558 | 1,504 | 1,496 |
| | | all | | | 2,000 | 2,000 |

- **`existing-vault`:** the 11 pairs are the same, because `qmd_knn` already found
  them, so this adds evidence and rank, not pairs.
- **`vault`:** its candidates are byte-identical in pair and generators. Its one
  inline link adds nothing, and `crates/kbx-gen/tests/generators.rs` holds its typed
  output to `tests/fixtures/typed-structure.json`, recorded before this change.
- **10,000 notes:** 70 pairs came in and 70 left. None of those that came in lacks a
  shared typed neighbour, so they are typed pairs that inline links lifted. The ones
  that left were at the bottom of the kept range, 0.8912 to 0.8915. No kept pair
  scored lower, and 124 scored higher.
- **With the index note:** 74 pairs came in and 74 left, again all with a shared
  typed neighbour.

| Vault | Wall before | Wall after | Peak before | Peak after |
|---|---|---|---|---|
| 10,000 notes | 5.4 s | 5.6 s | 2,254 MiB | 2,302 MiB |
| With the index note | 5.3 s | 5.6 s | 2,259 MiB | 2,320 MiB |

Most of the 2 GiB is `schema_gap` and the typed pairs, and it was there before.

## Limits of this evidence

- **The 10,000-note runs leave out `qmd_knn`,** which this change doesn't touch and
  which takes minutes. They use `--generators schema_gap,alias_cooccur,common_neighbors,adamic_adar,two_hop_bridge`.
  With it, kept counts would differ, as it competes for the same budget.
- **The synthetic vault is mostly typed.** It has 3,124 inline links to about 34,000
  typed rows, so it shows that typed pairs hold their place, not how good
  inline-only pairs are. `existing-vault` had 7 notes then. A real vault of inline links
  is the test still to come.
- **No precision.** No pair here has been reviewed, so `--eval` reports what's
  proposed and nothing about how many are right. Once a real vault has reviews,
  `--eval` at different weights is how to tune the default.

## The budget is shared

The second `~/HQ` run (`docs/e2e-hq.md`) kept 2,000 candidates, every one from
`alias_cooccur`, and dropped 10,424,689 over budget. One list, ranked by score,
lets whichever generator scores highest take it all. The budget is now shared:

- **Each generator with a pair is sure of a share.** A generator that has at
  least one pair left after the linked, rejected and locked filters gets
  `per_run / n` of the run, with the remainder one each to the first in the
  order they ran. `type_pair_prior` only re-ranks, so it has no share.
- **A pair counts once.** It takes one place in the run however many found it,
  and it counts against the share of each generator that found it.
  `common_neighbors`, `adamic_adar` and `two_hop_bridge` find many of the same
  pairs, and this keeps the three of them from having three shares' worth
  between them.
- **Unused shares go to the others.** A generator that finds fewer pairs than
  its share, or whose pairs hit the per-note limit, leaves the rest of the run to
  the best pairs not yet kept, from any generator. The total stays at `per_run`.
- **A pair found by two ranks above what either gives it.** The combined score
  is `1 − Π(1 − squash(score))`, as before. A pair two generators found isn't
  put above every pair one found: `common_neighbors` and `adamic_adar` find the
  same pairs by construction, and that rule would hand them the top of the queue.
- **The same vault gives the same run.** Pairs are taken by combined score,
  then `cid`.

There's no setting for this in `kbx.toml`: equal shares, with what's unused passed
on, needs none. `--budget` still sets `per_run`.

`kbx suggest` shows what each generator found and kept. "Found" is the pairs
that passed the filters to compete for the budget; a pair two generators found
counts for both, so the column doesn't add up to the total. `kbx suggest`
prints no JSON, so the text summary is the only place this appears.

```
2000 candidates
  generator             found    kept
  schema_gap                0       0
  alias_cooccur          2481     500
  common_neighbors       3613    1500
  adamic_adar            3613    1500
  two_hop_bridge         3613    1500
dropped 0 linked or rejected, 4094 over budget
```

## `alias_cooccur`'s score

It used to score a pair by how many times the body said the term, which the union
put at 0.5 for one mention and close to 1.0 for a few. A one-word project name
said four times in passing outranked a two-word name said once in a note about
it, and a title such as "Overview", shared by five notes, made five pairs from
every note that used the word.

Now each term (a title or alias, by its words) has a **specificity** from 0 to
1, the product of:

- **Rarity:** `ln((N + 1) / (df + 1)) / ln(N + 1)`, where `N` is the notes in
  the index and `df` the notes whose body says the term. A term said by one note
  in 10,000 scores about 0.92; one said by a fifth of them, 0.17.
- **Length:** `(characters + 4 × (words − 1)) / 16`, up to 1, counting the
  spaces. "Ark" is 0.19, "kubernetes" 0.63, "token bucket" 1.
- **One over the notes it names.** A title that five notes share says a fifth as
  much about each of them.

A pair's chance is `1 − Π(1 − specificity × (1 − 0.2^mentions))` over the terms
naming the target that the source says: one mention of a fully specific term is
0.8, two 0.96. The generator reports the odds, `chance / (1 − chance)`, as its
raw score, so the union's `squash` turns it back into that chance. The evidence
gains `mentions`, the times the terms were said; `terms` and `src` are unchanged.

**At most 50 pairs per term** (`MAX_PAIRS_PER_TERM`), from the notes that say it
most, then by id. Ten million pairs is mostly a few broad terms each said by
thousands of notes, and their pairs all say the same thing at a low score. The
cap was chosen over dropping broad terms, so the notes that keep coming back to a
project still reach the queue. Rarity still counts every note that says the term.

What it costs: a note that says a broad term no more than the 50 it keeps loses
its pair, even if the note is about the project. On the vault below, that is 45
true pairs, the notes about each of the three projects, which say the name three
times against passing notes that say it up to four.

## Measurements

`tools/gen-alias-vault.py` writes a vault with known truth, and labels for
`kbx import reviews`:

- **Narrow:** one note in 20 is a topic with a two-word alias, such as "quartz
  ledger", said one to three times by two to six notes about it, which share a
  `part_of` group. Those pairs are true.
- **Broad:** projects named Atlas, Orion and Nova, said in passing one to four
  times by 20%, 8% and 3% of notes (false), and three times each by 15 notes about
  them (true).
- **Common titles:** five notes each titled Overview, Summary, Status, Planning,
  Meeting and Update, whose words 60% of bodies use up to three times (false).

Before is `13c9a50`, after is this change. Each run is
`kbx suggest --judge none` on an emptied `candidates` table, with no `qmd_knn`.
"Top 100" is the kept pairs by stored score.

**10,000 notes, alias_cooccur alone:**

| | Before | After |
|---|---|---|
| Found, over budget | 49,712 | 2,481 |
| Kept | 2,000 | 2,000 |
| True among kept | 699 | 2,000 |
| True in top 100 | 0 | 100 |
| Top 100 scores | 0.8000 to 0.9375, 6 distinct | 0.8455 to 0.8737, 2 distinct |

Before, the top 100 are all passing mentions said four times. After, they are
all narrow aliases. The narrow pairs have similar `df` and length here, so
the top scores are close together; the spread is between kinds of term, not
within them.

**10,000 notes, all five generators:**

| Generator | Found before | Kept before | Found after | Kept after |
|---|---|---|---|---|
| `schema_gap` | 0 | 0 | 0 | 0 |
| `alias_cooccur` | 49,712 | 11 | 2,481 | 500 |
| `common_neighbors` | 3,613 | 1,989 | 3,613 | 1,500 |
| `adamic_adar` | 3,613 | 1,989 | 3,613 | 1,500 |
| `two_hop_bridge` | 3,613 | 1,989 | 3,613 | 1,500 |
| Over budget | | 51,325 | | 4,094 |
| True in top 100 | | 91 | | 100 |
| True among kept | | 1,989 | | 2,000 |

Before, the 11 `alias_cooccur` pairs that got in scored above the structural
pairs, and all 11 were false. After, it keeps its 500 and the structural group
keeps the rest, ranked above it by score.

**`kbx suggest --eval`, labels imported:**

| Vault | Generator | Proposed | Hits | Precision | Recall |
|---|---|---|---|---|---|
| 10,000 notes, before | `alias_cooccur` | 49,712 | 2,076 | 0.04 | 0.36 |
| 10,000 notes, after | `alias_cooccur` | 2,481 | 2,031 | 0.82 | 0.36 |
| fixture, before and after | `alias_cooccur` | 2 | 1 | 1.00 | 0.33 |

The structural generators are unchanged. The fixture vault and its 7 labels in
`tests/fixtures/bakeoff/synthetic-labels.jsonl` are the only labelled pairs in
the repo; they give the same numbers before and after, as the fixture has one
alias pair. `tests/fixtures/bakeoff/pairs.jsonl` has no labels yet.

**Cost at 80,000 notes,** `--generators alias_cooccur`, on this machine:

| Vault | Over budget before | After | Wall before | After | Peak before | After |
|---|---|---|---|---|---|---|
| `gen-alias-vault.py --notes 80000` | 387,202 | 2,449 | 18.9 s | 2.9 s | 890 MiB | 97 MiB |
| `gen-vault.py`, 80,000 files, 64,000 indexed | 0 | 0 | 57 to 75 s | 48 s | 215 to 220 MiB | 237 MiB |

On the alias vault, true pairs among the 2,000 kept go from 694 to 2,000, and in
the top 100 from 0 to 100. The pairs over budget were being built, with their
evidence, and thrown away; now a term stops at 50. On `gen-vault.py`'s vault
`alias_cooccur` finds nothing either way, as its aliases end in a number no body
says, and the time is reading 64,000 bodies. The 22 MiB more is the terms held by
words. A run of all five generators there didn't finish in 30 minutes before this
change, and `schema_gap` and the structural generators, which this doesn't touch,
are most of that, so it isn't in the table.

## Limits of this evidence

- **Synthetic labels.** The truth is what the generator wrote, and it was written
  to have broad and narrow terms. A real vault's terms sit between, and the next
  `~/HQ` run is the check.
- **The top-100 spread is narrow.** Every top pair here is a narrow alias said
  three times, so the top 100 hold two distinct scores. What the score separates
  is kinds of term: the broad and common ones fall below 0.5.
- **No `qmd_knn`.** It competes for the same budget and gets its own share.
