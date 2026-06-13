# core api

## entry point

```rust
let index = lepiter_core::KnowledgeBase::open("./lepiter")?;
```

returns a `KnowledgeBaseIndex` with:

- `pages: HashMap<PageId, PageMeta>`
- `index_issues: Vec<ParseIssue>`

## lazy page loading

```rust
let page = index.load_page(page_id)?;
```

only the requested page file is parsed.

## page keys

use `PageId` as the canonical key.
filenames are treated as storage detail/fallback.

## node model

the block-oriented model includes:

- `Heading`
- `Paragraph`
- `Text`
- `List`
- `Code`
- `Link`
- `Quote`
- `Unknown`

`Unknown` preserves unsupported schema variants without crashing consumers.

## utilities

- `render_page_to_text(&Page) -> String`
- `render_nodes_to_text(&[Node]) -> String`
- `page_content_contains(&Page, needle) -> bool` — streaming per-node content check (no full-page allocation)
- `collect_node_types_in_file(path) -> HashMap<String, usize>`

## probe example

```bash
cargo run -p lepiter-core --example probe -- ./lepiter
```

the probe prints:

- page ids and titles (sorted)
- observed node types with counts
- parse/index failures
