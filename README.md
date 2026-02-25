# lepiter-cli

terminal tools for reading lepiter knowledge bases stored as page json files.

this repository is a cargo workspace with two crates:

- `lepiter-core`: resilient parser, metadata index, and plain text renderer.
- `lepiter-cli` (package in `lepiter-tui/`): read-only terminal ui for browsing and reading pages.

`./lepiter/` is fixture/input data (the gt book knowledge base), not source code.

## features

- indexes pages by canonical page id (`PageId`), not filename.
- lazy loading: metadata first, full page parsing on demand.
- resilient parsing: unknown source node types are preserved as `Node::Unknown`.
- probe example to inspect corpus shape and parse issues.
- tui with full-screen list/search, full-screen page reading, markdown-like rendering, and internal link navigation.

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

list/search view:

- `j/k` or `up/down`: move selection
- `enter`: open selected page
- `/`: start search
- `esc`: return to list view
- `q`: quit

page view:

- `j/k` or `up/down`: scroll
- `tab` / `shift+tab`: select next/previous link
- `enter`: follow selected link
- `h`: go back in page-link history
- `b` or `esc`: return to list view

## documentation

- architecture: [`docs/architecture.md`](docs/architecture.md)
- core api guide: [`docs/core-api.md`](docs/core-api.md)
- tui behavior: [`docs/tui.md`](docs/tui.md)
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
