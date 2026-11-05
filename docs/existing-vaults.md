# Existing vaults

kbx needs nothing from an existing vault. A note with no `id`, `type` or `title`
is still indexed: kbx infers what the file doesn't state and keeps that in the
index. Indexing, linting and suggesting write nothing to your notes.

## What is inferred

Each key is settled on its own, and a key in the frontmatter always wins. A key
that is present but empty counts as absent. A file with no frontmatter is a note
with all three inferred.

| Key | Inferred from |
|---|---|
| `id` | the path from the vault root, see below |
| `title` | the first H1 with text in it, or else the file name without `.md` |
| `type` | `default_type` in `kbx.toml`, for the note's scope or else for the vault |

`kbx index` counts the notes it inferred something for, apart from those that
failed: `…, 6 inferred`. A file that can't be read as a note, such as one with
invalid YAML, still fails.

Inference only covers files kbx reads. What's [excluded](excluding.md), such as
`node_modules` or a path in `.gitignore`, isn't a note, so now that a file needs
no frontmatter to be indexed, `exclude` is how to keep one out.

## The id rule

An inferred id is the path's slug, then `-`, then the first 8 hex digits of the
SHA-256 of the path:

```
Projects/Alpha Plan.md  →  projects-alpha-plan-a5262ca6
```

The path has `/` separators and keeps its case and its `.md`. The slug is the
path without `.md`, lowercased, with its runs of letters and digits joined by
`-` and cut to 64 characters. A path with no letters or digits has the slug
`note`.

- **It's stable.** It depends on the path alone, so no other note can change it,
  and every run gets the same one.
- **It's safe from collisions.** `a/b.md`, `a-b.md` and `A/B.md` share a slug and
  differ in the digest. If two files still claim one id, the lowest path keeps it
  and `kbx lint` reports `id.duplicate`, as for ids you write yourself.
- **It follows the file.** Moving or renaming a note gives it a new id. Its open
  candidates are gone, and its rejections and strikes stay with the old id. To
  keep an id through a rename, put `id:` in the note yourself.

## Types

`kbx init` writes the global class and one for each scope:

```toml
default_type = "Note"  # the class of a note with no `type` key

[scope]  # per-scope `default_type`
People = { default_type = "Entity" }
Projects = { default_type = "Note" }
```

A `kbx.toml` from before this has neither, and every note gets `Note`. Changing
either makes the next `kbx index` rebuild.

The example ontology has a `Note` class. `see_also`, `part_of` and `contradicts`
accept any class, so notes of the default class can take those from the start.
The other relations name their classes, so give a scope a more specific class,
or widen a relation, to use them.

`kbx doctor` has a `types` row. It warns if a `default_type` isn't a class in your
ontology, as in a vault made before `Note` was added, and says what to add. It
also lists the values of your notes' own `type` keys that the ontology lacks,
most common first. Those notes are indexed with the class they state, `kbx lint`
reports them as `type.unknown`, and relations that accept any class still work.

## What gets written

Only `kbx accept` writes to a note with an inferred key, and it writes the
relation alone: see [the frontmatter writer](frontmatter-writer.md#notes-with-inferred-keys).
`kbx suggest --stage` leaves such a note alone, and its candidates stay in
`kbx review`. There is no migration command.

## Links

Links resolve as usual, so `[[Alpha Plan]]` in a body becomes a `mentions` edge.
`kbx lint` doesn't call a link by filename `link.unstable` when the target's id
is inferred: the path is all that id stands on, so there's nothing steadier to
link by.
