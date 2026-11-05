# The frontmatter writer

The writer is off by default. `kbx accept` then keeps the relation in `.kbx/` and
[prints the diff](review.md#accepted-relations) of what writing it would do, and no
note changes. To have kbx write to your notes, add this to `.kbx/kbx.toml`:

```toml
[write]
frontmatter = true
```

With that, `kbx accept`, `a` in `kbx review`, [staging](staging.md) and
`kbx.reviewed` write as this page says. `kbx accept --no-write` keeps one run's
relations in `.kbx/` anyway. This page says what gets written, what's refused, and
where a comment can be lost. The README lists
[everything kbx can write](../README.md#what-kbx-writes-and-where).

## Applying what `.kbx/` holds

Relations accepted while the setting was off stay in `.kbx/` when you turn it on.

```sh
kbx apply --dry-run   # print the diff, change nothing; needs no setting
kbx apply             # write them; needs the setting
```

`kbx apply` prints the diff and writes each relation with this same writer, so a
note ends up as if you'd accepted with the setting on: the inverse, the
`qmd.metadata` mirror, and [the rules for inferred keys](#notes-with-inferred-keys)
all apply. A written relation leaves `.kbx/` and is a frontmatter relation from
then on.

Accepted relations don't go stale the way candidates do, so `kbx apply` allows an
edit `kbx index` has seen: the note on disk has to match the index. An edit it
hasn't seen is refused, like the other refusals [below](#what-is-refused). What's
refused stays in `.kbx/`, the rest is written, and the command exits 1. Run
`kbx index` and `kbx apply` again.

## What changes

Accepting a candidate adds `rel: ["[[dst-id]]"]` to the note holding the
relation and the inverse to the other note. Both notes also get `type` and
`tags` mirrored into `qmd.metadata`, for `qmd --filter`.

A note with an inferred `id`, `type` or `title` gets less: see
[Notes with inferred keys](#notes-with-inferred-keys).

Only the text between the `---` fences changes. The fences, a BOM, the body and
its line endings stay byte for byte. New lines use the line ending of the line
before them, so a CRLF file stays CRLF.

The writer edits the text rather than parsing and re-serializing it, because
`serde_yaml` keeps no comments and no formatting. So every key stays in place,
and every comment outside `qmd.metadata` stays. A new relation key goes after
your last key, before `kbx:` and `qmd:`. A value is extended in its own style:

| Before | After |
|---|---|
| `rel: ["[[a]]"]  # why` | `rel: ["[[a]]", "[[b]]"]  # why` |
| a `- "[[a]]"` list, at any indent | one more `- "[[b]]"` line at that indent |
| `rel: "[[a]]"` | `rel: ["[[a]]", "[[b]]"]` |
| `rel:` or `rel: []` | `rel: ["[[b]]"]` |

Keys kbx doesn't define, such as a `meta:` block another tool wrote, keep their
bytes, order, quoting and comments, whatever they hold. kbx ignores them when it
reads a note, and a nested map under one doesn't stop the note from indexing.

## Notes with inferred keys

An accepted relation is the only thing written to a note that leaves out `id`,
`type` or `title` ([what's inferred](existing-vaults.md)). The rule is the same
for the note holding the relation and for the note taking its inverse, and for
any key missing, not just all three:

- The inferred keys are not written. The inferred id lives in the graph alone.
  It comes from the path, so every `kbx index` gets the same one without it.
- `qmd.metadata` isn't written or updated, since it would state kbx's guess.
- A link **to** a note with an inferred id is written by path, without `.md`:
  `see_also: ["[[Projects/Alpha Plan]]"]`, not the id. Obsidian follows that link
  and rewrites it when you rename the note, and the index resolves it to the same
  id. If the path can't be spelled as a link (it has `#`, `|`, `^`, `[` or `]`) or
  doesn't lead to exactly this note, the link uses the id, which only kbx follows.

A note with no frontmatter gets a block holding the relation:

```
---
see_also: ["[[Projects/Alpha Plan]]"]
---
```

It goes first in the file, after a BOM if there is one, with the line ending of
the file's first line. The body stays byte for byte. In a block with only `kbx:`
or `qmd:` keys, or none, the relation goes first.

A note stating all three keys is written to as described above, and a link to it
uses its id.

## Where a comment is lost

`kbx:` and `qmd:` are tool-owned (§3), and these are the only places a comment
can go:

- When `qmd.metadata` is out of date, it is written again as one line. Comments
  on it or inside it are dropped. Other keys under `metadata`, and other lines
  under `qmd:`, stay.
- When `qmd:` is itself one `{…}` map, that line is written again, with the same
  result for a comment on it.
- When kbx changes `kbx:`, the whole block is written again from its value, in
  the form §3 shows, because an editor may have written it back in any style.
  [Staging](staging.md) does this. Comments in the block are dropped. Keys kbx
  doesn't know stay.

If `qmd.metadata` is already current, the block isn't touched at all. An
acceptance leaves `kbx:` alone unless the candidate was staged.

## What is refused

Every edit is checked by parsing the result: it must equal the old frontmatter
plus the change, with the keys in the same order. If it doesn't, the edit is
refused and the file is left alone, rather than falling back to a rewrite that
would drop comments. That covers YAML a line-based edit can't follow, such as
keys indented as a whole or a multi-line string with text at the left margin.
`rel: ~` and a relation behind a YAML anchor are refused the same way. Add the
link by hand instead.

Acceptance is also refused, with nothing written and with the setting on or off,
when:

- Either note's `content_hash` differs from the one recorded when the candidate
  was generated. This holds after `kbx index` too. Run `kbx index` and
  `kbx suggest` again.
- The target note is gone (`target.unavailable`) or the source is
  (`source.unavailable`).
- The notes' classes fit the relation in neither direction (`rel.range`).
- The relation is `exclusive` and already has another target, in a note or kept in
  `.kbx/` (`rel.cardinality`).
- The key holds something other than wikilinks.

Candidates that share a note can be accepted in one go: `kbx accept a b` updates
the recorded hash of `b` when `a` writes to a note they share.

## Writes

A write goes to a temp file in the note's folder (`.kbx-*.tmp`), takes the
note's permissions and is renamed over it, so a reader sees the old file or the
new one. A symlink stays a symlink. Both files of a pair are prepared before
either is renamed. If the second rename fails, the first note gets its old text
back. A hard link, or ownership and extended attributes on the old file, do not
survive the rename.

## Direction and inverses

The schema decides direction (§3). If the classes fit only the other way round,
the relation is written the other way round. If they fit both ways, the
candidate's direction stands.

To keep an inverse out of your notes, mark the relation in `ontology.yaml`:

```yaml
relations:
  depends_on: {inverse: required_by, domain: [Procedure], range: [any], weight: 0.9, derived_only: true}
```

Accepting then writes the source note alone, and `kbx index` derives the
`required_by` edge.

## Section anchors

With `write.section_anchors = true` a section-level candidate writes its anchor
in the target, as in `rel: ["[[dst-id-slug#^block-id]]"]`. The body still isn't
touched: kbx never adds a block id. It's off by default, because some tools break
on an anchored link. See [Section-level relations](sections.md).

## Which relation

`kbx accept` uses `--rel`, or else the latest verdict that names a relation. If
there is neither, as after `--judge none`, it asks for `--rel`.

## Rejections and strikes

`kbx reject` records a row in `reviews`, adds a strike and takes the candidate
out of the queue. Without `--rel` or a verdict the row's `rel` is `none`.

Strikes count against the pair, in either direction and across all relations.
Below three, a later `kbx suggest` can propose the pair again. At three, no
generator does until `kbx unreject <cid>`, and `kbx unreject --list` prints the
suppressed pairs. Unrejecting clears the strikes and keeps the rows and their
reasons, because `reviews` is the evaluation set for the generators.

`kbx defer` records the decision and leaves the candidate in the queue.

`kbx review` takes the same decisions one candidate at a time: see
[Reviewing candidates](review.md).
