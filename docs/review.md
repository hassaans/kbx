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
| `a` | Accept: the same as `kbx accept`. With no relation yet, it asks for one first. |
| `r` | Reject: asks for a reason, which can be empty, then the same as `kbx reject`. |
| `d` | Defer: the same as `kbx defer`. The candidate is back in the next session. |
| `e` | Edit the relation, picked from the ontology's `judge.vocabulary`. Nothing is recorded until you then press `a`, `r` or `d`. |
| `s` | Skip, recording nothing. The candidate is back in the next session. |
| `q` | Quit, as does Ctrl-C. Decisions already taken stand. |

The session is one pass through the queue and ends at the last candidate.

If the index can't follow an acceptance, the links are still in your notes:
`kbx review` says so when it ends, exits 1, and `kbx index` repairs it.

A decision that is refused, for example because a note changed since
`kbx suggest`, shows why in red and leaves the candidate where it is. See
[the frontmatter writer](frontmatter-writer.md) for what gets refused.

## Accepting without writing

```sh
kbx accept <cid>… --rel see_also --no-write
```

This takes the decision and leaves every note alone, for a vault where you want to
see the change first or where kbx may not write at all. It has every check and
refusal of `kbx accept`. The decision goes in `reviews` as an acceptance, the
candidate is marked as accepted, and a unified diff of each note goes to stdout:

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

The flag isn't called `--dry-run` because `kbx suggest --dry-run` leaves the
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

### What happens to it next

| Command | What it does |
|---|---|
| `kbx accept <cid>` | Writes it, under the relation already accepted. `--rel` changes the relation, and the earlier one's row goes from `reviews`. |
| `kbx accept <cid> --no-write` | Prints the diff again against the notes as they are now. |
| `kbx reject <cid>`, `kbx defer <cid>` | Take the acceptance back, and then do what they always do. |
| `kbx suggest` | Leaves the decision in place, and doesn't stage it. A run that finds the pair again moves the candidate's recorded hashes up to the notes. |

The hash guard still applies. A note edited after the diff refuses the write,
until `kbx index` and `kbx suggest` have run. `kbx accept --no-write` then shows
the new diff. If you link the two notes yourself, the next `kbx suggest` drops the
candidate and the decision in `reviews` stands.

`kbx review` keeps such a candidate in the queue, in its place by score, with
`accepted, not written: a writes it` in the header. So `a` there writes it, as any
`a` writes to your notes: `--no-write` has no key.

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
[staged](staging.md) in your notes. Accepting or rejecting one here takes its
line out of the note.

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

A candidate accepted with `--no-write` also has `"accepted_unwritten": true`, and
`rel` is the relation accepted. Any other candidate has no such key.

A section-level candidate also has `src_anchor` and `dst_anchor`, where it has an
anchor at that end, such as `01KBX…06#^burst`. See
[Section-level relations](sections.md). A note-level one has neither key.

Act on it with `kbx accept`, `kbx reject` and `kbx defer`.
