# Staged proposals

`kbx suggest --stage` puts proposals in your notes, so that you can decide on
them in your editor. `kbx sync-stage` takes what you did there as decisions. It
is another way into [the review queue](review.md), not a second queue.

## Staging

```sh
kbx suggest --stage
```

This does what `kbx suggest` does, then writes each candidate of the run under
`kbx.proposed` in the note a relation would be written to:

```yaml
kbx:
  proposed:
    - {rel: see_also, to: "[[01J9Z8…pkce]]", by: [adamic_adar, judge], score: 0.81, id: 7f3a…}
```

| Key | What it says |
|---|---|
| `rel` | The relation proposed, or `none` if there's no judge to name one. |
| `to` | The note it would point at, with its anchor if [section anchors](sections.md) are on. |
| `by` | The generators that found the pair, and `judge` if a judge accepted it. |
| `score` | The signals combined, from 0 to 1, to two places. |
| `id` | The candidate's cid, which `kbx accept` and `kbx reject` take. |

What's staged is what `kbx review` would show from that run: a candidate hidden
for repeating a rejected relation isn't staged. The candidates stay in the
database, so `kbx review` and `kbx review --json` show the same queue either way,
and accepting or rejecting there takes the line out of the note.

Staging again adds only what's new. A line you removed isn't put back, even
before `kbx sync-stage` has seen that it's gone.

Staging goes through [the frontmatter writer](frontmatter-writer.md): nothing
outside the frontmatter changes, and only `kbx:` and the `qmd.metadata` mirror
change inside it. A note edited since `kbx index` takes no entries, and the
command exits 1. The other notes are still staged.

`kbx index` never reads `kbx.proposed`. The graph is the same with or without
it, apart from the line numbers of whatever comes after the block.

## Deciding in your editor

```sh
kbx sync-stage
kbx sync-stage --mark-reviewed
```

`kbx sync-stage` indexes the vault first, then goes through what it staged:

| You | It means |
|---|---|
| Removed the line | A rejection of that relation: a row in `reviews` and a strike, the same as `kbx reject`. |
| Linked the two notes under a relation key | An acceptance of that relation: the inverse is written, the review recorded and the line taken out. |
| Left the line | Nothing. The candidate is still in the queue. |

For an acceptance the relation is the key you used, whatever `rel` said, and it
reads the way you wrote it. That's how to accept a `rel: none` proposal. You can
write the link on either note, with an id, a filename or an alias, and it counts
whether or not you removed the line.

Three rejections suppress a pair, as they do from `kbx reject`.

### What isn't a rejection

Only a line that's gone from a note kbx can read is a rejection. These wait, and
are looked at again on the next run:

- A note that's been deleted, or whose frontmatter doesn't parse.
- A `kbx.proposed` that no longer reads as proposals, such as an entry that has
  lost its `id`.

An editor that writes the block back in another style changes nothing, because
entries are read as YAML and matched by `id`.

### What is refused

These are reported, exit 1, and stay as they are until you fix them:

- A link under a key the ontology doesn't know, as after a typo.
- A key the schema reads the other way round, such as `depends_on` on a note
  outside its domain. It isn't turned around, because that would write something
  other than what you wrote.
- The other refusals of [`kbx accept`](frontmatter-writer.md#what-is-refused).

### The hash guard

`kbx accept` refuses a note that changed since `kbx suggest`. An acceptance by
`kbx sync-stage` doesn't, because you made it on the notes as they are now. Both
notes must only match the index, which it has just brought up to date.

Removing a line changes the file, but not the note's content, so the other
candidates on that note can still be accepted. If you changed anything else, or
moved a line into a relation key, they are refused until `kbx suggest` runs
again, as after any edit.

If a line's candidate has been decided in `kbx review`, or dropped because the
pair is now linked or suppressed, `kbx sync-stage` removes the line and records
nothing.

## The `kbx.reviewed` lock

```yaml
kbx:
  reviewed: 2026-11-03
```

A note with `kbx.reviewed` gets no new proposals until its content changes. No
generator's pair at that note is kept, at either end: the schema can turn a pair
around, and accepting writes to both notes. `kbx suggest` says how many it left
out. Candidates stored before the lock stay in the queue.

`kbx sync-stage --mark-reviewed` sets it to today, in UTC, on each note where
that run took a decision and left no line to decide. Nothing sets it otherwise.

The lock goes by the note's content, recorded by `kbx index` when it first sees
the date, so `kbx index` has to have seen both the date and any edit:

- `kbx:` and `qmd:` don't count, so staging, syncing and mirroring leave the lock
  in place.
- Anything else does, in the frontmatter or the body. That includes a relation
  kbx writes, so accepting a candidate at a locked note lifts the lock.
- A lifted lock stays in the file with its old date, and does nothing. Marking
  the note again sets a new baseline, even on the same day.
