# Export, import and the learning loop

`reviews` is the record of what you decided, so kbx learns from it, scores its
generators against it, and lets you take it elsewhere.

## `kbx export`

All three print to stdout and change nothing.

```
kbx export edges --jsonl      # the edges table
kbx export reviews --jsonl    # the reviews table
kbx export rdf                # the graph as Turtle
```

`--jsonl` is the default, and so far the only format. Rows come in key order, one
JSON object per line. Every field is always there, `null` when empty. Field
names only ever get added to.

| Export | Fields |
|---|---|
| `edges` | `src`, `dst`, `rel`, `inverse_of`, `weight`, `origin`, `src_section`, `line` |
| `reviews` | `src`, `dst`, `rel`, `decision`, `reason`, `strikes`, `decided_at` |

`edges` holds every row, mirrors of inverses and `mentions` included.

## `kbx import reviews`

```
kbx import reviews --jsonl reviews.jsonl
kbx export reviews --jsonl | ssh other 'cd vault && kbx import reviews --jsonl'
```

It reads a file, or stdin without one, and adds to the table:

- One transaction: a line that can't be read is reported by number and nothing
  is imported. A field kbx doesn't know is refused, not dropped.
- A row already there is replaced unless its `decided_at` is newer, so importing
  an old backup can't undo later decisions, and importing twice changes nothing.
- Rows the file doesn't mention stay.
- `strikes` come across, so imported rejections suppress pairs as they did.
- Notes are not touched. An imported `accept` writes no link: the row is a
  record of the decision, and the link is in the notes it came with.

It needs no index, and reviews of notes this vault doesn't have are kept.

## RDF

`kbx export rdf` writes Turtle with every IRI in full.

| Thing | IRI |
|---|---|
| Note | `urn:kbx:note:<id>`, percent-encoded |
| Class | `https://github.com/hassaans/kbx/ns#<class>` |
| Relation | its `iri` in the ontology, or `https://github.com/hassaans/kbx/ns#<relation>` |
| Mention | `https://github.com/hassaans/kbx/ns#mentions` |

Each note gets `a <class>` and an `rdfs:label` from its title. Relations are
stated the way the ontology names them, once each: `required_by` rows mirror
`depends_on` ones and aren't exported, and a symmetric relation is stated from
both ends. Frontmatter keys outside the ontology are left out.

To map a relation to a vocabulary, give it an `iri` (or `uri`, the same thing) in
`ontology.yaml`:

```yaml
relations:
  see_also: {iri: "http://www.w3.org/2004/02/skos/core#related", inverse: see_also, domain: [any], range: [any], weight: 0.3, symmetric: true}
```

It has to be absolute, and an ontology with one that isn't is refused.

## `type_pair_prior`

This generator proposes nothing. It re-ranks what the others found, before the
budget is applied, using how pairs of the same two types fared in `reviews`. It
runs by default; leave it out of `--generators` to go without. With no reviews it
does nothing.

For each relation a pair of types can hold (the schema has to allow it), it
estimates:

```
(accepts + 1) / (accepts + rejects + 2)
```

A rejection that names no relation, as after `--judge none`, counts against all
of them. A candidate takes the best relation's estimate, since no judge has
named one yet, and its score moves by those odds. An estimate of 1/2 moves
nothing, and a type pair nobody has reviewed isn't touched at all.

- Types count as a pair, not in order: Procedure-Concept and Concept-Procedure
  are the same evidence. The schema decides direction, and a rejection records
  the order the candidate happened to be in.
- Deferrals teach it nothing, and neither do reviews of notes no longer in the
  index.
- `kbx unreject` doesn't change it: that lifts a suppression, and the rejection
  stays on record.

A candidate it moved lists `type_pair_prior` in `generators`, and `features`
holds `score`, `rel`, `accepts` and `rejects`. In `why_proposed` it reads
`type_pair_prior:0.67`.

## `kbx suggest --eval`

```
$ kbx suggest --eval
generator          proposed reviewed  hits  precision  recall
schema_gap               40        3     1       0.33    0.25
common_neighbors         12        5     3       0.60    0.75
against 4 accepted pairs and 3 rejected pairs
```

It changes nothing, and takes `--generators` but not the flags that limit or
write. For each generator:

- **proposed**: the pairs it finds.
- **reviewed**: those with an accept or a reject. The rest say nothing either
  way, since nobody has looked at them.
- **hits**: those with an accept for the pair and relation. The relation is the
  latest verdict's; a candidate with no verdict matches on the pair.
- **precision**: hits over reviewed. `-` if nothing it found was reviewed.
- **recall**: hits over accepted pairs. `-` if there are none.

An accepted pair is linked and its candidate is gone, and a suppressed one is
never proposed, so run on the vault as it stands no generator could find a
reviewed pair. `--eval` therefore runs on a temporary copy of the database,
without the links between accepted pairs, without strikes, and with no budget.
Reviews of notes not in the index don't count, since no generator could find
them.

Three limits:

- Links accepted later were not there when earlier pairs were proposed, and
  links other than accepted ones are as they are now, so the copy approximates
  the vault as it was. `alias_cooccur` and `qmd_knn` read the notes as they are.
- `qmd_knn` searches the qmd index as it stands, because `--eval` doesn't refresh
  it. Run `kbx suggest` first, or its row will show what a stale or empty index
  finds.
- `type_pair_prior` isn't scored. It finds no pairs, and it learned from the rows
  it would be scored on.
