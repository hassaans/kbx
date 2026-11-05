# kbx

kbx is a command-line tool that turns a markdown vault into a typed knowledge
graph. It suggests links between notes, judges them with LLMs, and writes the
relations you accept back to frontmatter. It's built for Obsidian and other
personal knowledge management users.

kbx is in early development. So far it compiles a vault into a graph, checks it
against the ontology and answers questions about it:

```sh
kbx init                   # create .kbx/, kbx.toml and the example ontology
kbx index                  # compile changed notes into .kbx/graph.sqlite
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
kbx suggest --stage        # also write proposals to `kbx.proposed` for your editor
kbx sync-stage             # take edits there as decisions; see docs/staging.md
kbx suggest --eval         # each generator's precision and recall against your reviews
kbx watch                  # re-index saved notes and lint them, until stopped
kbx watch --suggest        # also run `kbx suggest` 45 s after the last save
kbx hook install           # lint staged notes on commit; prints an Obsidian recipe
kbx review                 # go through the candidates: a / r / d / e / s / q
kbx review --json          # the same queue for editors and agents
kbx accept <cid>… --rel see_also   # write relations and inverses to frontmatter
kbx reject <cid>… --reason "…"     # three rejections of a pair suppress it
kbx defer <cid>…           # put a decision off
kbx unreject --list        # the suppressed pairs; `kbx unreject <cid>` lifts one
kbx export edges --jsonl   # the edges table, one JSON object per line
kbx export reviews --jsonl # the reviews table; `kbx import reviews --jsonl` reads it back
kbx export rdf             # the graph as Turtle; see docs/export.md
kbx bakeoff                # score judge models against labels; see docs/judge-bakeoff.md
kbx doctor                 # check the vault, the index, RAM, the judges, qmd and its collections
kbx doctor --fix           # create the qmd collections that are missing
```

The pre-commit hook lints staged notes as they are on disk, so a file edited
after `git add` is linted as edited.

Every lint finding is an error except `link.unstable`, a link by filename rather
than id. That's a warning, because it's how most Obsidian vaults link.

`kbx init` and `kbx doctor` install [QMD](https://www.npmjs.com/package/@tobilu/qmd),
the search backend, with `npm i -g @tobilu/qmd` if it isn't on `PATH`. That needs
Node >= 22.

Each vault gets its own qmd index, with one collection per scope, so your other
qmd collections are left alone. `qmd.roots` in `kbx.toml` says which folder each
collection covers. `kbx suggest` brings the index up to date first, which
downloads qmd's models (about 2 GB) the first time, then runs one search per
note, the least linked first, at most 100 per run. Those searches are most of the
run's time, so kbx keeps what they found in `.kbx/search-cache.sqlite` and repeats a
search only when its note, or a section it found, has changed. Delete that file to
search afresh; [docs/embedding-cache.md](docs/embedding-cache.md) has the details.
Set `qmd.index` to use an index of your own; kbx then searches it but leaves updating
it to you.

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
