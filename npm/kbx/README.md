# kbx

Turn a markdown vault into a typed knowledge graph.

```sh
npm i -g kbx
kbx doctor
```

This package is a small launcher. npm installs the prebuilt binary for your
machine through an optional dependency, with no install scripts, so
`--ignore-scripts` works. Supported targets:

| Target                 | Package               |
| ---------------------- | --------------------- |
| macOS arm64            | `kbx-darwin-arm64`    |
| macOS x64              | `kbx-darwin-x64`      |
| Linux x64 (glibc)      | `kbx-linux-x64-gnu`   |
| Linux arm64 (glibc)    | `kbx-linux-arm64-gnu` |
| Windows x64            | `kbx-windows-x64`     |

Anywhere else, including Alpine and other musl systems, use `cargo install kbx`.

Docs and source: <https://github.com/hassaans/kbx>. Licensed under AGPL-3.0.
