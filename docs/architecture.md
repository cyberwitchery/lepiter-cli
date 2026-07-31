# architecture

## overview

the project separates parsing/modeling concerns from terminal rendering.

`lepiter-core`:

- scans `.lepiter` files
- builds `KnowledgeBaseIndex` (`HashMap<PageId, PageMeta>`)
- lazily parses page content to `Page`
- normalizes recursive source json into a stable block model (`Node`)
- resolves attachment paths relative to kb root (`AttachmentResolver`)
- optional external renderers handle unknown snippet types via ipc

`lepiter-cli` (package in `lepiter-tui/`):

- consumes `lepiter-core` api
- implements the cli subcommands (`info`, `list`, `search`, `show`, `links`, `tags`, `check`, `export`, `import`)
- keeps a filtered list of page ids
- caches rendered pages after first open
- supports link-driven navigation, search, backlinks, and snippet editing

## data flow

1. `KnowledgeBase::open(path)` scans metadata.
2. list mode shows sorted metadata.
3. opening a page calls `KnowledgeBaseIndex::load_page(id)`.
4. tui renders normalized nodes and caches the rendered result.
5. link navigation resolves to page ids and opens lazily.

## resilience rules

- unknown snippet/node types are never fatal.
- unknown types map to `Node::Unknown { typ, raw }`.
- non-fatal indexing issues are stored in `KnowledgeBaseIndex::index_issues`.

## performance notes

- the tui avoids eager parsing of all pages.
- parsing and render caching are per opened page.
- the index stores metadata and file paths only.
