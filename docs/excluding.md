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
  to the repository's root. `include` can bring these back, and only these.

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

## `include`

A `.gitignore` says what stays out of git, which isn't always what should stay out of
kbx. `include` lists what a `.gitignore` doesn't get to keep out. It sits beside
`exclude`, above the first `[table]` in `kbx.toml`:

```toml
include = ["/personal", "/archive/2024/*.md", "!/personal/taxes"]
```

Entries are `.gitignore` patterns from the vault root, as in `exclude`, but they match
only in the case they're written. A pattern that matches a folder covers everything in
it, and a pattern with `!` takes a path out again. The list is empty unless you set it.
A pattern that can't be read stops the command.

The order is the same everywhere:

1. A hidden path is left alone, whatever `include` says.
2. So is anything in `exclude`, including what's in a folder `include` covers.
3. What `include` covers is read, whatever any `.gitignore` says, including one nested
   inside the folder it covers.
4. What's left is read unless a `.gitignore` ignores it.

`include` brings things back and nothing more: it adds nothing to what's read in a vault
with no `.gitignore`, and it's not a list of the only things to read.

Unlike git, `include` can reach inside an ignored folder. With `personal/` in
`.gitignore` and `personal/journal` in `include`, kbx goes through `personal` to reach
`journal`, and everything else in `personal` stays ignored. A pattern with no folder to
start from, such as `journal` or `*.keep.md`, can match at any depth, so kbx has to read
every ignored folder to look for it. Start a pattern with `/` or a folder name to spare
it that on a big vault.

`kbx doctor` shows the effect before any run. Its `gitignore` row counts the markdown
files `.gitignore` files remove and how many of those `include` brings back, in the same
way the index does: what's hidden or in `exclude` is in neither number. It warns if
`include` is set and brings nothing back, which usually means a typo.

```
gitignore     ok   .gitignore files remove 11193 markdown files; `include` brings back 3027
```

## What's not read

`.git/info/exclude` and git's global ignore file don't count. They belong to one clone
or one machine, so two copies of a vault would index differently. A vault can also use
them to keep nearly everything out of git, which says what git tracks, not what's
private.

`.gitignore` does count, so a vault that keeps notes out of git that way keeps them out
of kbx too, until `include` says otherwise. `kbx init` lists the top-level folders it
skipped. A note inside a folder that isn't skipped goes without a word, though `kbx
doctor` counts it; `KBX_LOG=debug` shows notes named on the command line that are passed
over.

## When the rule changes

- **The index:** the next `kbx index` drops what's newly excluded and reads what `include`
  newly brings back. A run on a named path does so too for that path.
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

`include` changes the globs in three ways:

- **A folder `include` covers** is read like any other: it's in the glob, and anything
  excluded under it is gone around.
- **A folder only partly brought back** is read for those notes alone. With `personal/`
  in `.gitignore` and `include = ["personal/journal"]`, a collection of `personal` gets
  `journal/**/*.md`. Notes directly in such a folder get `*.md` if `include` brings back
  all of them, or else one entry each, so nothing a `.gitignore` still ignores reaches
  qmd. If nothing's brought back, the glob is `.kbx-no-notes.md`, which matches nothing.
- **A top-level folder `.gitignore` took out** isn't in `scopes`, because `kbx init`
  skipped it. Once `include` brings notes back from one, `kbx doctor` reports its
  collection as missing and `kbx doctor --fix` makes it. `scopes` doesn't change. A vault
  with no scopes has one collection already, and only its glob changes.

A glob is fixed when it's made, so a change to `include` shows up as an outdated glob
until `kbx doctor --fix` makes it again. A new note in a partly brought back folder is
the same if its glob lists notes one by one. The built-in engine has no globs: it reads
what's in the graph, so the next `kbx index` is all it needs.

`kbx doctor` warns about any collection of an excluded folder or with an outdated glob.
`kbx doctor --fix` removes the first kind and makes the second again, which has qmd read
that collection afresh. An index named in `qmd.index` is left as it is, but kbx stops
searching a collection of an excluded folder there. Nested folders in such an index are
still read by qmd, and their hits come to nothing as above.
