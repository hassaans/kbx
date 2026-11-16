# kbx

kbx is a command-line tool that turns a markdown vault into a typed knowledge
graph. It suggests links between notes, judges them with LLMs, and keeps the
relations you accept in `.kbx/`, or writes them to frontmatter if you turn that on.
It's built for Obsidian and other personal knowledge management users.

kbx is in early development. So far it compiles a vault into a graph, checks it
against the ontology and answers questions about it:

```sh
kbx init                   # create .kbx/ with kbx.toml and the example ontology
kbx index                  # compile changed notes into .kbx/graph.sqlite; says why any were left out, and exits 1 if the index ends up with no notes
kbx index --full           # rebuild, ignoring the stored hashes
kbx index notes/a.md       # limit the run to some files and folders
kbx lint                   # check the vault; exits 1 if there are errors
kbx lint notes/a.md        # report on the files you changed
kbx lint --strict          # treat warnings as errors
kbx lint --format json     # findings as JSON; `kbx lint --help` lists the codes
kbx graph neighbors <id>   # the typed edges at a note
kbx graph path <a> <b>     # a shortest typed path between two notes
kbx graph stats            # node, edge and section counts
kbx suggest                # store link candidates from every generator
kbx suggest --dry-run      # counts and token estimates; the database is left alone
kbx suggest --note <id> --generators schema_gap,alias_cooccur
kbx suggest --budget 50    # keep at most 50 candidates
kbx suggest --judge none   # stop at the candidates; by default a logged-in claude or codex CLI judges them, else local ollama or llama-server
kbx suggest --budget-usd 0.50   # stop judging before this run spends more (see docs/judge.md)
kbx suggest --embed skip   # search the vectors there are and keywords; embed nothing (see docs/search-engine.md)
kbx suggest --stage        # with writing on, also put proposals in `kbx.proposed` for your editor
kbx sync-stage             # with writing on, take edits there as decisions; see docs/staging.md
kbx suggest --eval         # each generator's precision and recall against your reviews
kbx watch                  # re-index saved notes and lint them, until stopped
kbx watch --suggest        # also run `kbx suggest` 45 s after the last save
kbx hook install           # lint staged notes on commit; prints an Obsidian recipe
kbx review                 # go through the candidates: a / r / d / e / s / q
kbx review --json          # the same queue for editors and agents
kbx review --accepted      # the relations accepted and kept in .kbx/
kbx accept <cid>… --rel see_also   # add relations and inverses to the graph, print the diff, touch no note
kbx accept <cid>… --no-write       # the same with writing on, for this run
kbx apply --dry-run        # the diff of writing the relations kept in .kbx/ to frontmatter
kbx apply                  # with writing on, write them
kbx mark-reviewed <id>…    # no new proposals for these notes until their content changes
kbx reject <cid>… --reason "…"     # three rejections of a pair suppress it
kbx defer <cid>…           # put a decision off
kbx unreject --list        # the suppressed pairs; `kbx unreject <cid>` lifts one
kbx export edges --jsonl   # the edges table, one JSON object per line
kbx export reviews --jsonl # the reviews table; `kbx import reviews --jsonl` reads it back
kbx export rdf             # the graph as Turtle; see docs/export.md
kbx bakeoff                # score judge models against labels; see docs/judge-bakeoff.md
kbx doctor                 # check the vault, the index, RAM, the judges and search
kbx doctor --fix           # move a root kbx.toml into .kbx/; on QMD, create missing collections
```

The pre-commit hook lints staged notes as they are on disk, so a file edited
after `git add` is linted as edited.

Every lint finding is an error except `link.unstable`, a link by filename rather
than id. That's a warning, because it's how most Obsidian vaults link.

### Existing vaults

kbx needs nothing from an existing vault: `kbx init` and `kbx index` work on the
notes you have. A note can leave out `id`, `type` and `title`, or have no
frontmatter at all. kbx infers what's missing and keeps it in the index:

- `id` from the note's path, as a slug and a short hash
- `title` from the first H1, or else the filename
- `type` from `default_type` in `kbx.toml`, per scope, with `Note` as the fallback

Anything your frontmatter states wins. `kbx doctor` says what to add to the
ontology for classes it lacks, such as the values of your own `type` keys.

kbx writes nothing to your notes unless you [turn writing on](#writing-to-frontmatter).
With it on, accepting a link writes the relation and nothing else to a note with
inferred keys. Renaming such a note gives it a new id, so add `id:` to any note
whose history should survive that. See [Existing vaults](docs/existing-vaults.md).

### What kbx writes, and where

Everything kbx keeps for a vault is under `.kbx/`: the config, the database, the
ontology `kbx init` starts you with, the search index and the caches. That
includes what it learns about your notes: accepted relations and their inverses,
review locks, and the `id`, `type` and `title` it infers. `kbx init`, `kbx index`,
`kbx suggest`, `kbx accept` and `kbx review` change nothing outside `.kbx/`.

An accepted relation is an edge like any other, with `kbx` for its origin rather
than `frontmatter`. It survives `kbx index --full`, generators see the pair as
linked, `kbx lint` counts it, and `kbx export` emits it. `kbx review --accepted`
lists them, and [Accepted relations](docs/review.md#accepted-relations) has the
rest. If a note later states the same relation, the note wins and `.kbx/` lets go
of it.

kbx looks for `.kbx/kbx.toml` first, then `kbx.toml`, in the current folder and
then each parent. A `kbx.toml` at the vault root, as earlier versions wrote, still
works. `kbx doctor` warns about it and `kbx doctor --fix` moves it into `.kbx/`,
where it reads the same: a relative `vault` still starts at the folder holding
`.kbx/`. If one folder has both, kbx stops with an error naming them and you keep
one. `kbx init --in-kbx` is still accepted, and does what `kbx init` does.

### Writing to frontmatter

To have kbx write to your notes, add this to `.kbx/kbx.toml`:

```toml
[write]
frontmatter = true
```

Then `kbx accept` and `a` in `kbx review` write the relation and its inverse to
frontmatter, and [`kbx suggest --stage`, `kbx sync-stage`](docs/staging.md) and the
`kbx.reviewed` lock work. Without the setting the staging commands refuse and say
so. See [the frontmatter writer](docs/frontmatter-writer.md).

Relations you accepted before turning it on stay in `.kbx/` until you run
`kbx apply`, which writes them the same way. `kbx apply --dry-run` prints the diff
first, and works with the setting off.

Nothing below happens unless you run the command beside it, and neither `kbx watch`
nor `kbx watch --suggest` runs any of them. The rows about notes need the setting.

| Path | Written by |
|---|---|
| `kbx.toml` at the vault root | Nothing creates one now. If you have one, `kbx init --search qmd` rewrites its qmd collections, and `kbx doctor --fix` moves it. |
| Notes: relations, inverses and the `qmd.metadata` mirror | `kbx accept` without `--no-write`, `a` in `kbx review`, `kbx apply`, and `kbx sync-stage` for a link you moved under a relation key |
| Notes: `kbx.proposed` | `kbx suggest --stage` adds lines. `kbx sync-stage`, `kbx accept`, `kbx reject`, and `a` and `r` in `kbx review` take out the lines of what they decide. |
| Notes: `kbx.reviewed` | `kbx sync-stage --mark-reviewed` and `kbx mark-reviewed` |
| `.kbx-*.tmp` beside a note | Any of the three rows above, for the moment before the rename over the note |
| `pre-commit` in the repository's hooks folder, usually `.git/hooks/` | `kbx hook install` |
| The file named by `--doc`, by default `docs/judge-bakeoff.md` from where you run it | `kbx bakeoff`, between its markers |
| The file named by `--out` | `kbx bakeoff --prepare --out` |

Outside the vault:

| Path | Written by |
|---|---|
| `kbx/models/` in the user's cache folder, or `KBX_MODEL_DIR` | `kbx suggest` on the built-in engine, which downloads its model once |
| qmd's own index and config, such as `~/.cache/qmd/` and `~/.config/qmd/`, which hold the text of your notes | On QMD: `kbx init`, `kbx doctor --fix` and `kbx suggest` |
| qmd itself, in npm's global folder | On QMD: `kbx init` and `kbx doctor`, if it's missing |
| A temporary folder, removed afterwards | `kbx suggest` and `kbx bakeoff`, when a judge CLI runs |

### Search

`kbx suggest` has two search backends, and `search` in `kbx.toml` says which.
`kbx doctor` prints the one in use.

**`search = "builtin"`** is what `kbx init` writes. It's kbx's own engine, BM25 and a
local embedding model, kept in `.kbx/search-index.sqlite`. It needs no Node and no
qmd. It downloads the model (134 MB) on first use, and with no network it searches by
keyword and says so. [docs/search-engine.md](docs/search-engine.md) has the details.

**`search = "qmd"`** uses [QMD](https://www.npmjs.com/package/@tobilu/qmd). Pick it if
your notes aren't in English, since the built-in model reads English only, or if you
already run QMD and want its index. Start a vault with `kbx init --search qmd`. `kbx
init` and `kbx doctor` then install it with `npm i -g @tobilu/qmd` if it isn't on
`PATH`. That needs Node >= 22, which nothing else in kbx does.

A vault made by an earlier kbx says `search = "qmd"` and stays on QMD. A `kbx.toml`
with no `search` key uses QMD if it lists qmd collections, and the built-in engine if
not. To move a vault, change the key; each backend keeps its own index.

On QMD, each vault gets its own qmd index, with one collection per scope, so your other
qmd collections are left alone. `qmd.roots` in `kbx.toml` says which folder each
collection covers. `kbx suggest` brings the index up to date first, which
downloads qmd's models (about 2 GB) the first time, and embeds new notes if that
should take under 15 minutes. It says how much it's about to embed, and `--embed wait`
or `--embed skip` decide for it ([details](docs/search-engine.md#when-a-run-embeds)).
Then it runs one search per
note, the least linked first, at most 100 per run. Those searches are most of the
run's time, so kbx keeps what they found in `.kbx/search-cache.sqlite` and repeats a
search only when its note, or a section it found, has changed. Delete that file to
search afresh; [docs/embedding-cache.md](docs/embedding-cache.md) has the details.
Set `qmd.index` to use an index of your own; kbx then searches it but leaves updating
it to you.

kbx leaves some folders alone, as if they weren't there: hidden ones, what `.gitignore`
files ignore, and what `exclude` in `kbx.toml` lists, which starts with `node_modules`
and folders named like secrets such as `credentials`. They aren't scopes, aren't
indexed, and no judge sees them. `kbx init` says which top-level folders it skipped.
[docs/excluding.md](docs/excluding.md) has the rule and how to change the list.

Logs go to stderr. Set `KBX_LOG=debug` to see each file that's skipped, and pass
`--log json` to get one JSON object per line.

## Install

The npm package is `kbx-cli`, and the command it installs is `kbx`. It's out from
v0.1.3, so `npm i -g kbx-cli` won't work before that release.

```sh
npm i -g kbx-cli
```

npm installs a prebuilt binary through an optional dependency. There are no
install scripts, so `--ignore-scripts` works. Prebuilt binaries cover macOS arm64
and x64, Linux x64 and arm64 with glibc, and Windows x64. Anywhere else, including
Alpine and other musl systems, use Cargo:

```sh
cargo install kbx
```

## Build from source

```sh
git clone https://github.com/hassaans/kbx
cd kbx
cargo build --workspace
```

To try the npm packages, pack them from a built binary and run their tests. Change
`kbx-darwin-arm64` to your platform's package in `npm/`:

```sh
npm/pack.sh kbx-cli dist
npm/pack.sh kbx-darwin-arm64 dist target/debug/kbx
KBX_BINARY=target/debug/kbx node --test "npm/test/*.test.js"
```

## License

[AGPL-3.0](LICENSE)
