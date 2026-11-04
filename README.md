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
kbx lint                   # check the vault; exits 1 if there are findings
kbx lint notes/a.md        # report on the files you changed
kbx lint --format json     # findings as JSON; `kbx lint --help` lists the codes
kbx graph neighbors <id>   # the typed edges at a note
kbx graph path <a> <b>     # a shortest typed path between two notes
kbx graph stats            # node, edge and section counts
kbx doctor                 # check the vault, the index, RAM and qmd
```

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

```sh
cargo install kbx
```

## Build from source

```sh
git clone https://github.com/hassaans/kbx
cd kbx
cargo build --workspace
```

## License

[AGPL-3.0](LICENSE)
