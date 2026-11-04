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
kbx suggest --judge auto   # also judge them with the logged-in claude or codex CLI
kbx watch                  # re-index saved notes and lint them, until stopped
kbx watch --suggest        # also run `kbx suggest` 45 s after the last save
kbx hook install           # lint staged notes on commit; prints an Obsidian recipe
kbx review                 # go through the candidates: a / r / d / e / s / q
kbx review --json          # the same queue for editors and agents
kbx accept <cid>… --rel see_also   # write relations and inverses to frontmatter
kbx reject <cid>… --reason "…"     # three rejections of a pair suppress it
kbx defer <cid>…           # put a decision off
kbx unreject --list        # the suppressed pairs; `kbx unreject <cid>` lifts one
kbx doctor                 # check the vault, the index, RAM, the judges and qmd
```

The pre-commit hook lints staged notes as they are on disk, so a file edited
after `git add` is linted as edited.

Every lint finding is an error except `link.unstable`, a link by filename rather
than id. That's a warning, because it's how most Obsidian vaults link.

`kbx init` and `kbx doctor` install [QMD](https://www.npmjs.com/package/@tobilu/qmd),
the search backend, with `npm i -g @tobilu/qmd` if it isn't on `PATH`. That needs
Node >= 22.

Logs go to stderr. Set `KBX_LOG=debug` to see each file that's skipped, and pass
`--log json` to get one JSON object per line.

## Install

Nothing is published yet, so these commands won't work until the first release.

```sh
npm i -g kbx
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
npm/pack.sh kbx dist
npm/pack.sh kbx-darwin-arm64 dist target/debug/kbx
KBX_BINARY=target/debug/kbx node --test "npm/test/*.test.js"
```

## License

[AGPL-3.0](LICENSE)
