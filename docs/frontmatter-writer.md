# The frontmatter writer

`kbx accept` writes accepted relations to your notes. This page says what it
writes, what it refuses, and where it can lose a comment. To see all that without
a note changing, use [`kbx accept --no-write`](review.md#accepting-without-writing).
The README lists [everything kbx can write](../README.md#what-kbx-writes-and-where).

## What changes

Accepting a candidate adds `rel: ["[[dst-id]]"]` to the note holding the
relation and the inverse to the other note. Both notes also get `type` and
`tags` mirrored into `qmd.metadata`, for `qmd --filter`.

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

Acceptance is also refused, with nothing written, when:

- Either note's `content_hash` differs from the one recorded when the candidate
  was generated. This holds after `kbx index` too. Run `kbx index` and
  `kbx suggest` again.
- The target note is gone (`target.unavailable`) or the source is
  (`source.unavailable`).
- The notes' classes fit the relation in neither direction (`rel.range`).
- The relation is `exclusive` and already has another target (`rel.cardinality`).
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
