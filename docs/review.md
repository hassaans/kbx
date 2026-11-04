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

A section-level candidate also has `src_anchor` and `dst_anchor`, where it has an
anchor at that end, such as `01KBX…06#^burst`. See
[Section-level relations](sections.md). A note-level one has neither key.

Act on it with `kbx accept`, `kbx reject` and `kbx defer`.
