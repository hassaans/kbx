# Reviewing candidates

`kbx review` goes through the candidates `kbx suggest` stored, one at a time. It
needs a terminal. `kbx review --json` prints the same queue and changes nothing.

## The queue

Candidates come best score first. A candidate whose latest verdict is `unsure`
goes to the end, unranked, whatever its score. Candidates the judge rejected
stay in the queue, in score order, with the verdict showing.

Each one shows both notes' excerpts, what proposed it (`why_proposed`, one entry
per generator), and the judge's verdict and rationale if there is one. The
relation shown is the one a decision would use: the latest verdict that names
one, unless you change it. It reads the way accepting would write it, which the
schema may turn around from the order the generators found the pair in.

| Key | What it does |
|---|---|
| `a` | Accept: the same as `kbx accept`, so by default the relation is kept in `.kbx/`. With no relation yet, it asks for one first. |
| `r` | Reject: asks for a reason, which can be empty, then the same as `kbx reject`. |
| `d` | Defer: the same as `kbx defer`. The candidate is back in the next session. |
| `e` | Edit the relation, picked from the ontology's `judge.vocabulary`. Nothing is recorded until you then press `a`, `r` or `d`. |
| `s` | Skip, recording nothing. The candidate is back in the next session. |
| `q` | Quit, as does Ctrl-C. Decisions already taken stand. |

The session is one pass through the queue and ends at the last candidate.

With [writing on](frontmatter-writer.md), if the index can't follow an acceptance,
the links are still in your notes: `kbx review` says so when it ends, exits 1, and
`kbx index` repairs it.

A decision that is refused, for example because a note changed since
`kbx suggest`, shows why in red and leaves the candidate where it is. See
[the frontmatter writer](frontmatter-writer.md) for what gets refused.

## Accepted relations

```sh
kbx accept <cid>… --rel see_also
```

By default this takes the decision and leaves every note alone. With
`[write] frontmatter = true` it writes to the notes, and `--no-write` gets this
behaviour for one run. It has every check and refusal of accepting with the
setting on. The decision goes in `reviews` as an acceptance, the relation goes in
`.kbx/graph.sqlite`, and a unified diff of what writing it would do goes to stdout:

```diff
accepted 6f1c…, not written: 01KBX…02 -see_also-> 01KBX…03
--- a/engineering/01KBX…02-oauth-flow.md
+++ b/engineering/01KBX…02-oauth-flow.md
@@ -3,4 +3,7 @@
 type: Procedure
 title: OAuth flow
+see_also: ["[[01KBX…03]]"]
+qmd:
+  metadata: {type: Procedure, tags: []}
 ---
```

`--no-write` isn't called `--dry-run` because `kbx suggest --dry-run` leaves the
database alone, and this doesn't.

The diff is the frontmatter writer's own output against the file on disk, so it's
everything accepting would write, byte for byte, line endings included:

- **The inverse** is a diff of its own, on the other note, because that's a
  separate file with a change of its own. With `derived_only` there is no second
  diff, and a line says `kbx index` derives the inverse.
- **The `qmd.metadata` mirror** is in the diff of each note it would change.
- **A staged candidate's line** shows as removed from `kbx.proposed`.
- **Candidates in one run** that share a note make one diff for that note, as
  accepting them in that order would write it. Separate runs each show their
  change against the file as it is now, so a later diff doesn't include an earlier
  one that's still unwritten.

On a terminal, control characters in the diff are dropped, as in `kbx review`.
Piped, it's exact.

### What it is in the graph

An accepted relation is an edge with origin `kbx`, and its inverse is a `derived`
edge, as for a relation in frontmatter. A `derived_only` relation behaves the same.

- **`kbx index`** keeps it, through `kbx index --full` and a rebuild for a new
  version of kbx. It lives in its own table, which only a decision changes.
- **Generators** see the pair as linked, so it isn't proposed again.
- **`kbx lint`** counts it towards what a class `requires`.
- **`kbx export`** emits it, and the `origin` field says where it came from.
- **`kbx graph`** walks it, and `kbx graph stats` counts it.
- **An `exclusive` relation** counts a target kept in `.kbx/`.

`kbx review` doesn't ask about it again. `kbx review --accepted` lists what `.kbx/`
holds, with `--json` for tools, and `kbx review` says how many there are.

If a note stops existing, the edge goes with it and the relation waits in `.kbx/`
until the note is back. A note with an inferred id gets a new id when it's renamed,
so a relation kept for it waits for good: `kbx review --accepted` still lists it,
`kbx apply` reports `source.unavailable` or `target.unavailable`, and
`kbx reject <cid>` clears it. Put `id:` in a note to keep its relations through a
rename.

### When a note states the relation

Frontmatter wins. If `kbx index` finds either note stating the relation, from
either end, there is one relation, of origin `frontmatter`, and `.kbx/` drops its
copy. The note owns it from then on: taking the link out of the note takes the
relation out of the graph. The decision in `reviews` stands.

### What happens to it next

| Command | What it does |
|---|---|
| `kbx accept <cid>` | With the setting off, prints the diff again, and `--rel` changes the relation. With it on, writes it, under the relation already accepted, or `--rel`'s. The earlier relation's row goes from `reviews`. |
| `kbx accept <cid> --no-write` | The same as with the setting off. |
| `kbx apply` | Writes everything `.kbx/` holds, with the setting on. See [the writer](frontmatter-writer.md#applying-what-kbx-holds). |
| `kbx reject <cid>`, `kbx defer <cid>` | Take the acceptance back, edges included, and then do what they always do. |
| `kbx suggest` | Leaves the decision in place, and doesn't stage it. A run that finds the pair again moves the candidate's recorded hashes up to the notes. |

The hash guard still applies to `kbx accept`, but a relation kept in `.kbx/` is
checked against the index: a note that has changed and been indexed since is fine,
and one edited since `kbx index` refuses the write. `kbx accept --no-write` shows
the diff against the note as it is.

With the setting off, `kbx review` leaves these candidates out of the queue. With
it on, it keeps them in their place by score, with
`accepted, not written: a writes it` in the header. So `a` there writes it, as any
`a` writes to your notes then: `--no-write` has no key.

## What is left out

A pair with one or two strikes comes back on the next `kbx suggest`, often with
the cached verdict you just rejected. So a candidate proposing a relation
already rejected for its pair, in either direction, is hidden from `kbx review`
and `kbx review --json`. The header counts them. It returns if a new verdict
names another relation, or after `kbx unreject <cid>`. `kbx accept` and
`kbx reject` still work on its cid.

A rejection that named no relation, as after `--judge none`, hides nothing:
there is no relation to tell apart, so the pair is asked about again and its
strikes can reach three.

## Staged candidates

The queue is the same whether or not candidates are
[staged](staging.md) in your notes. With writing on, accepting or rejecting one
here takes its line out of the note. With it off, the line stays until you turn it
back on and run `kbx sync-stage`.

## `kbx review --json`

An array, in queue order. A field with nothing to say is `null`. `src` and `dst`
are the candidate's own order: the schema decides the direction on accepting.

```json
[
  {
    "cid": "6f1c…",
    "src": "01KBX000000000000000000002",
    "dst": "01KBX000000000000000000003",
    "src_title": "OAuth flow",
    "dst_title": "Tokens",
    "rel": "depends_on",
    "score": 0.82,
    "generators": ["adamic_adar", "common_neighbors"],
    "verdict": "accept",
    "rationale": "The flow cannot run without tokens."
  }
]
```

With writing on, a candidate accepted with `--no-write` also has
`"accepted_unwritten": true`, and `rel` is the relation accepted. Any other
candidate has no such key.

`kbx review --accepted --json` is an array of what `.kbx/` holds, each with `cid`,
`src`, `dst`, `rel` and `decided_at`.

A section-level candidate also has `src_anchor` and `dst_anchor`, where it has an
anchor at that end, such as `01KBX…06#^burst`. See
[Section-level relations](sections.md). A note-level one has neither key.

Act on it with `kbx accept`, `kbx reject` and `kbx defer`.
