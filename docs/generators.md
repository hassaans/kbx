# Structural generators and inline links

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
