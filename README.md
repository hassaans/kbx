# kbx

kbx is a command-line tool that turns a markdown vault into a typed knowledge
graph. It suggests links between notes, judges them with LLMs, and writes the
relations you accept back to frontmatter. It's built for Obsidian and other
personal knowledge management users.

kbx is in early development. So far it compiles a vault into a graph:

```sh
kbx init                # create .kbx/, kbx.toml and the example ontology
kbx index               # compile changed notes into .kbx/graph.sqlite
kbx index --full        # rebuild, ignoring the stored hashes
kbx index notes/a.md    # limit the run to some files and folders
```

Set `KBX_LOG=debug` to see each file that's skipped.

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
