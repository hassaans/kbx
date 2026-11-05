# What kbx leaves alone

Some folders in a vault aren't notes: `node_modules`, a `credentials` folder, a build
output. kbx never reads them. They don't become scopes or qmd collections, they aren't
indexed or linted, they stay out of both search engines, and no judge sees a word of
them. That last part matters most, since a cloud judge is sent note excerpts.

One rule decides this. `kbx init`, `kbx index`, `kbx lint` (and so the pre-commit hook),
`kbx watch`, `kbx suggest` and `kbx bakeoff` all use it.

## The rule

A file or folder is left alone if it, or a folder it's in, is one of these:

- **Hidden:** its name starts with a dot.
- **In `exclude`:** the list in `kbx.toml`.
- **Ignored by a `.gitignore`:** in the vault, with or without a repository. If the
  vault is a folder inside a repository, the `.gitignore` files above it count too, up
  to the repository's root.

## `exclude`

`kbx init` writes the defaults:

```toml
exclude = ["node_modules", ".git", ".obsidian", ".kbx", "credentials", "secrets", ".ssh", ".aws", ".gnupg"]
```

Entries are `.gitignore` patterns from the vault root, but they match in any case, so
`secrets` also covers `Secrets`. A bare name matches at any depth, and a pattern with a
slash starts at the vault root:

```toml
exclude = ["node_modules", "credentials", "secrets", "tmp", "team-x/legal", "*.private.md"]
```

The key replaces the defaults, so keep the ones you want when you add your own. `exclude
= []` turns the list off, but hidden folders and `.gitignore` files still count. A
`kbx.toml` with no `exclude` gets the defaults, and `kbx doctor` says so. A pattern that
can't be read stops the command rather than running without it.

## What's not read

`.git/info/exclude` and git's global ignore file don't count. They belong to one clone
or one machine, so two copies of a vault would index differently. A vault can also use
them to keep nearly everything out of git, which says what git tracks, not what's
private.

`.gitignore` does count, so a vault that keeps notes out of git that way keeps them out
of kbx too. `kbx init` lists the top-level folders it skipped. A note inside a folder
that isn't skipped goes without a word; `KBX_LOG=debug` shows notes named on the command
line that are passed over.

## When the rule changes

- **The index:** the next `kbx index` drops what's newly excluded. A run on a named path
  does so too for that path.
- **`kbx suggest`:** it checks the graph first and re-indexes if it holds an excluded
  note, so a note can't get past the rule because nobody ran `kbx index`. The built-in
  search index then drops its text.
- **The judge:** the step that builds what a judge is sent applies the rule again,
  whatever the graph says. `kbx bakeoff` and `kbx suggest --dry-run` read the graph
  without changing it, so they rely on this.
- **`kbx watch`:** an edit to `kbx.toml` or a `.gitignore` starts a whole-vault pass.
- **`scopes`:** an excluded scope stays in `kbx.toml`, does nothing, and `kbx doctor`
  warns about it.

## QMD

qmd has no ignore list, only a glob for each collection, and `kbx init` writes one that
goes around excluded folders. A scope with nothing excluded under it gets `**/*.md`, and
one with `team-a/credentials/` gets a glob that lists the rest:

```
{*.md,other/**/*.md,sub/**/*.md}
```

Two limits follow:

- The glob is fixed when the collection is made. A new folder beside an excluded one
  isn't in it, so `kbx doctor` reports the collection until `kbx doctor --fix` makes it
  again. A new folder that's excluded can't get in by accident, unless the collection
  has nothing excluded in it yet and so still says `**/*.md`; `kbx doctor` reports that
  too.
- A glob can only go around folders. A single excluded file, such as one matching
  `*.private.md`, stays in qmd's local index. Its hits come to nothing, because it has
  no note in the graph.

`kbx doctor` warns about any collection of an excluded folder or with an outdated glob.
`kbx doctor --fix` removes the first kind and makes the second again, which has qmd read
that collection afresh. An index named in `qmd.index` is left as it is, but kbx stops
searching a collection of an excluded folder there. Nested folders in such an index are
still read by qmd, and their hits come to nothing as above.
