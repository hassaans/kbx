# Section-level relations

This page settles the §14 open question: how a relation can point at part of a
note, and what other tools do with such a link. It's off by default. To turn it
on, add this to `.kbx/kbx.toml`:

```toml
[write]
section_anchors = true
```

With the setting off, which is the default, kbx proposes, judges and writes
note-level pairs exactly as it did before. [What the tools do](#what-the-tools-do)
says why. An anchor reaches a note only with `frontmatter = true` under `[write]`
as well. Without it, an accepted section-level relation keeps its sections in
`.kbx/`.

## The convention

A section has three names, for three readers:

| Name | Form | Who it's for |
|---|---|---|
| Stable section key | `UUID5(id + heading path + ordinal)` | The index and `candidates`. Never written to a note. |
| Address | `{id}#^{block-id}`, or `{id}#{heading-slug-path}` if the section has no block id | People and agents: `kbx review` and `--json`. |
| Wikilink | `[[{id}-slug#^block-id]]`, or `[[{id}-slug#Heading#Subheading]]` | The note's frontmatter. |

`{id}#^{block-id}` is the portable form. A block id survives a heading being
renamed or moved, and Obsidian, Dataview and Breadcrumbs all parse `#^`.

The heading path is the fallback, and it has one difference between the address
and the wikilink. The address uses slugs (`#token-bucket/burst`), as §3 says. A
slug isn't something Obsidian can follow: it matches headings by their text, so
the wikilink spells the path the way Obsidian's docs do, one `#` per level and
the heading text as written (`#Token bucket#Burst`). Characters a wikilink can't
hold become spaces, as Obsidian's `stripHeadingForLink` does: `: # | ^ \ % [ ]`.

Which anchor a section gets, in order:

1. The first `^block-id` in the section. An id on the heading itself comes first,
   because it's on the section's first line. If a note repeats an id, only the
   first block holds it.
2. The heading path, unless the link would read the same as an earlier section's.
   A link can only reach the first of them. Headings that differ only in their
   slug, like `Rate-limits` and `Rate limits`, have different links, so both can
   be anchored, though both show the same address in review.
3. Nothing, as for text before the first heading with no block id. That end of
   the pair stays note-level.

The target is the file's name (`{id}-slug`) rather than the bare id, because
Obsidian looks links up by file name. kbx reads the id from the front of it, so
renaming the slug doesn't break the link for kbx. A file whose name doesn't start
with its id, or holds a character a wikilink can't, is linked by bare id.

kbx never adds a `^block-id` to a block that has none. That would edit the body,
which the [writer](frontmatter-writer.md) leaves byte for byte. To get a block
anchor where a heading link would go, add the id yourself and run `kbx index` and
`kbx suggest` again.

## What changes with the setting on

**Generators.** `qmd_knn` and `alias_cooccur` already know which part of a note
they found something in, and `kbx suggest` turns that into a section at each end:

- `qmd_knn`: the section of the hit, and on the note that searched, its longest
  section, since a note's search can't tell which section found the hit.
- `alias_cooccur`: at each end, the section where the terms occur most. A note
  often doesn't use its own alias anywhere, and then its end stays note-level.

If both found the pair, `alias_cooccur` decides an end it can place, because a
term in the text is exact evidence and a search hit isn't. The other generators
work on whole notes, so their pairs stay note-level.

**Candidates.** There's still one row per pair of notes, carrying the best section
pair in the nullable columns `candidates.src_section` and `dst_section`. If
neither end has an anchor, both are NULL and the row is the same as a note-level
one. Otherwise its `cid` also covers the two section keys, so a verdict about
these sections is never reused for other ones. When a later run settles on other
sections, the new row replaces the old one.

**Judge.** The judge sees each section as `excerpt`, instead of the start of the
note, with `id`, `type` and `title` still the note's. The prompt and schema don't
change, so `prompt_hash` doesn't either.

**Review.** `kbx review` shows the two section excerpts. `kbx review --json` adds
`src_anchor` and `dst_anchor` to an item that has them, as addresses. An item
without anchors has neither key.

**Writer.** `kbx accept` puts the anchor in the relation's target, and each note
gets the anchor of the section at the *other* end:

```yaml
# in the note holding the relation
depends_on: ["[[01KBX000000000000000000006-rate-limits#^burst]]"]
# in the target, if the relation has a written inverse
required_by: ["[[01KBX000000000000000000028-load-shedding#Shedding order]]"]
```

The anchor is worked out from the note as it is at that moment, which the
content-hash guard has just shown to be what the candidate was generated from.

**Staging.** `kbx suggest --stage` puts the same link in `to`, so moving the line
under a relation key keeps the anchor, and `kbx sync-stage` writes the inverse with
the anchor of the other end. `id` is the candidate's cid, section keys included.
If a later run settles on other sections, the new candidate is staged too. The old
line then has no candidate, so `kbx sync-stage` clears it, and removing it by hand
counts for nothing: remove the new line to reject the pair.

**Index.** There's no new link storage: the anchor stays in the target string,
and `kbx index` keeps it in `links.fragment`, as it always has. Links resolve on
the part before the `#`, so `edges`, `kbx graph` and the generators' linked-pair
check all stay note-level.

### Limits

- **One relation per pair of notes.** Once a pair is linked, no generator proposes
  it again, so a second section pair between the same notes needs a hand edit.
- **Strikes and rejected relations count per pair of notes** (§7), not per section
  pair. `kbx unreject` still takes the pair's cid, as `kbx unreject --list` prints
  it, not the cid of a section-level candidate.
- **A section isn't always the block.** The anchor is the section's first block,
  which may not be the sentence that matched if a section holds several.
- **`derived_only` relations** write no inverse, so the anchor on the source's side
  isn't recorded in any note.
- **Turning the setting off** takes effect at the next `kbx suggest`, which drops
  the rows with sections. Until then, those candidates can still be accepted with
  their anchors. Links already written stay: kbx doesn't rewrite them.
- **`kbx suggest --eval` and `--dry-run` estimates** work on note pairs, whatever
  the setting says.

## What the tools do

This is what a link like `depends_on: ["[[note#^id]]"]` does in each tool. Each row
says how it was checked: **source** means the project's published code was read at
the version given, **docs** means its documentation. **Nothing here was run inside
Obsidian**, because it isn't installed where this was written, and neither plugin
was executed.

| Tool | With an anchored link in a property | Checked by |
|---|---|---|
| kbx | Resolves to the note, keeps the anchor in `links.fragment`, and lints clean. | Tests in this repo |
| Obsidian: parsing | Indexes it in `frontmatterLinks`, with the anchor in `link` (`note#^id`), since 1.4.0. | Source: API typings 1.14.4 for the field, and the Dataview and Breadcrumbs code below, which reads anchors out of it |
| Obsidian: quoting | The value has to be a quoted string. kbx always quotes it. | Docs: Properties |
| Obsidian: link syntax | `#^id`, and one `#` per heading level, are the documented forms. Block ids are Latin letters, numbers and dashes, which is what kbx parses. | Docs: Internal links |
| Obsidian: click in Properties | **Unverified.** Expected to open the note at the block or heading, and at the top if the anchor is gone. | Not checked |
| Obsidian: backlinks and graph | **Unverified** for anchored links. 1.4.5 says Backlinks shows links in properties. `resolvedLinks` is keyed by file, so the graph should count the note. | Changelog and typings; not checked |
| Obsidian: renames | **Unverified.** Whether renaming the file or heading updates an anchored link in a property. | Not checked |
| Dataview 0.5.70: field value | Parsed into a link of type `block` or `header` that keeps its subpath, and renders as a link to it. | Source: `parseFrontmatter`, `Link.infer` |
| Dataview 0.5.70: `file.outlinks`, `FROM [[note]]` | Keeps the subpath in `file.outlinks`. `FROM [[note]]` goes by file path, so the note is found. | Source: `parsePage`, `FullIndex.finish` |
| Dataview 0.5.70: `=` and `contains()` | **Changes query results.** `compareValue` compares type and subpath, so `[[note#^id]]` isn't equal to `[[note]]`. `WHERE contains(depends_on, [[note]])` no longer matches. `meta(link).path` or `link(x.path)` compare the file. | Source: `Values.compareValue`, `contains` |
| Breadcrumbs 4.21.2 and later | Strips the anchor and makes an edge to the note, so the graph is note-level. | Source at 4.22.3: `resolve_relative_target_path`; changelog, #734 |
| Breadcrumbs before 4.21.2 | **Breaks.** The link becomes an edge to a phantom note `note#^id.md`, and the real note never sees the reverse edge. | Changelog 4.21.2, #734 |

Heading links behave like block links in every row, except that Dataview types them
`header`, and normalises the heading. Obsidian Bases is not covered: nothing here
was checked against it.

### Why the default is off

A plain `[[note]]` works everywhere. An anchored link breaks older Breadcrumbs,
which is 4.21.2 from July 2026, and quietly changes the result of existing
Dataview queries on every version. So no vault gets anchors without asking.

If you use Breadcrumbs, update it to 4.21.2 or later before turning the setting
on. If you use Dataview, check any query that compares a relation with a link.

### To check by hand in Obsidian

The rows marked **Unverified** need a real vault. In a test vault, with a note
`A` holding a `^x` block and a `## H` heading, and `B` with
`depends_on: ["[[A#^x]]", "[[A#H]]"]`:

1. Do both show as links in Properties, and does a click land on the block and
   the heading?
2. What does a click do after `^x` is removed from `A`?
3. Does `A` list `B` under Backlinks, and is there one edge in the graph?
4. After renaming `A`, is the property rewritten, with the anchor kept? After
   renaming the heading?

## A note on plain `[[id]]` links

`kbx accept` writes a note-level link as `[[{id}]]` (§7), with no slug. Obsidian
looks links up by file name, so for a file named `{id}-slug.md` that link
shouldn't resolve in Obsidian, though kbx resolves it. That's how it works today,
and it stays that way here, because note-level output must not change. Anchored
links use the file name, because an anchor is no use on a link that doesn't reach
the note. This is from reading the API, not from running Obsidian, so it belongs on
the same list as the checks above.

## Sources

- [Obsidian API typings](https://github.com/obsidianmd/obsidian-api/blob/master/obsidian.d.ts), 1.14.4
- [Obsidian: Internal links](https://obsidian.md/help/links) and [Properties](https://obsidian.md/help/properties)
- [Obsidian 1.4.5 changelog](https://obsidian.md/changelog/2023-08-31-desktop-v1.4.5)
- [Dataview source](https://github.com/blacksmithgu/obsidian-dataview), 0.5.70
- [Breadcrumbs source and changelog](https://github.com/SkepticMystic/breadcrumbs), 4.22.3
