# lepiter-cli

terminal tools for reading lepiter knowledge bases stored as page json files.

this repository is a cargo workspace with two crates:

- `lepiter-core`: resilient parser, metadata index, and plain text renderer.
- `lepiter-cli` (package in `lepiter-tui/`): terminal ui and cli subcommands for browsing, reading, and editing pages.

`./lepiter/` is fixture/input data (the gt book knowledge base), not source code.

## features

- indexes pages by canonical page id (`PageId`), not filename.
- lazy loading: metadata first, full page parsing on demand.
- resilient parsing: unknown source node types are preserved as `Node::Unknown`.
- probe example to inspect corpus shape and parse issues.
- tui with full-screen list/search, page reading, in-page search, backlinks, page creation, markdown-like rendering, and internal link navigation.
- optional external snippet plugins via ipc (see `docs/plugins.md`).
- tui editor for text and code snippets (auto-save with undo).
- cli subcommands: `info`, `list`, `ids`, `search`, `show`, `links`, `tags`, `check`, `export`, `import` (`lepiter-cli help` for details).

## quick start

from repository root:

```bash
cargo test
cargo run -p lepiter-core --example probe -- ./lepiter
cargo run -p lepiter-cli -- ./lepiter
cargo run -p lepiter-cli -- tui ./lepiter
```

install the cli binary locally:

```bash
cargo install --path lepiter-tui
lepiter-cli tui ./lepiter
```

print knowledge base metadata:

```bash
lepiter-cli ./lepiter
# or
lepiter-cli info ./lepiter
```

## tui keybinds

`j/k` move, `enter` open, `/` search, `n` new page, `e` edit, `B` backlinks,
`q` quit. press `?` in any mode for the full per-mode reference;
[`docs/tui.md`](docs/tui.md) lists every binding.

## documentation

- architecture: [`docs/architecture.md`](docs/architecture.md)
- core api guide: [`docs/core-api.md`](docs/core-api.md)
- plugin system: [`docs/plugins.md`](docs/plugins.md)
- tui behavior: [`docs/tui.md`](docs/tui.md)
- editor: [`docs/editor.md`](docs/editor.md)
- snippet support matrix: [`docs/snippet-support-matrix.md`](docs/snippet-support-matrix.md)

api docs from rustdoc:

```bash
cargo doc --no-deps --open
```

## workspace layout

- `lepiter-core/src/lib.rs`: public core api and parser implementation
- `lepiter-core/examples/probe.rs`: corpus probe utility
- `lepiter-core/tests/fixtures.rs`: fixture-driven tests
- `lepiter-tui/src/main.rs`: terminal ui and cli entrypoint implementation
- `lepiter/`: lepiter pages and attachments fixture corpus
