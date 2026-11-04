# kbx

kbx is a command-line tool that turns a markdown vault into a typed knowledge
graph. It suggests links between notes, judges them with LLMs, and writes the
relations you accept back to frontmatter. It's built for Obsidian and other
personal knowledge management users.

kbx is in early development: `kbx --version` is the only command so far.

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
