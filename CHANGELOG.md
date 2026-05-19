# changelog

all notable changes to this project are documented in this file.

## Unreleased

### added
- page creation from the TUI: press `n` in list mode to create a new page;
  type a title and press Enter to create a minimal `.lepiter` page (with a
  fresh UUID and an empty text snippet), then immediately enter edit mode
- snippet insertion in edit mode: press `Ctrl+A` to append a new empty text
  snippet to the current page; the cursor moves to the new snippet
- `KnowledgeBaseIndex::register_page()` public method to add a page to the
  index at runtime and re-sort the id list
- within-page content search: press `/` in page mode to search, `n`/`N` to
  cycle through matches, `Esc` to clear; matches are highlighted in the
  rendered page with the current match distinguished from other matches
- backlinks index: `KnowledgeBaseIndex::build_backlinks()` computes a reverse
  link index at load time, mapping each page to the set of pages that reference
  it via link nodes, inline `[label](target)` links, or `[[wiki]]` links
- `B` key in page mode displays incoming links (backlinks) for the current page
  in a navigable list; press Enter to open a linking page or Esc to return
- `extract_link_targets()` public function in lepiter-core extracts raw link
  target strings from a node tree (explicit links and inline markdown links)
- `O` key in page mode opens the current page via a user-configured external
  command (`LEPITER_OPEN_CMD`); the command receives the page id and file path as
  environment variables (`LEPITER_PAGE_ID`, `LEPITER_PAGE_PATH`)

### changed
- `LruCache` internals replaced: `HashMap` + `VecDeque` consolidated into a single `IndexMap`,
  eliminating duplicate key storage and improving cache-line behaviour for LRU promotion
- `extract_type`, `parse_heading`, and `is_code_snippet` are now public in lepiter-core;
  the TUI reuses them instead of maintaining separate copies
- the TUI now recognises `exampleSnippet`, `changesSnippet`, and
  `robocoderMetamodelSnippet` as editable code snippets (previously only 5 of 8 code
  snippet types were handled)
- syntax highlighting tokenizer no longer allocates a `String` per token or a `Vec<char>` per
  line; `CodeToken` now borrows `&str` slices directly from the source text

### fixed
- `show --open-links` now captures inline `[text](url)` and `[[wiki]]` links embedded
  in paragraph, heading, quote, and text nodes; previously only standalone link blocks
  were collected
- list items with multi-line content (code blocks, multiple paragraphs) now render all lines
  with proper indentation instead of truncating to the first line (TUI and plain-text renderers)
- edit-mode autosave now uses atomic writes (temp file + rename) to prevent page JSON
  corruption on crash or disk-full conditions
- `jsonSnippet` and `yamlSnippet` now render as code blocks with correct language
  tags instead of falling through to Unknown nodes
- transient plugin failures (timeout/crash) are no longer permanently cached in the LRU;
  subsequent renders for the same snippet retry the plugin instead of returning a stale error

## 0.7.0 - 2026-04-26

### changed
- editor paragraph lines now render full inline markdown (bold, italic, code, links)
  instead of only annotation highlighting
- `sorted_pages_by_title()` replaced by `sorted_pages()` which uses a cached title-sorted
  ordering computed once at `open()` time; `KnowledgeBaseIndex` now exposes `sorted_ids: Vec<PageId>`
- plugin child processes are now killed and reaped on exit (Drop impl for PluginProcess)
- `LruCache` accepts any key type implementing `Eq + Hash + Clone`, not just `String`
- plugin render cache now uses the shared `LruCache` instead of a hand-rolled implementation
- search filtering and title sorting no longer allocate a temporary String per page on every
  keystroke; the lowercased title is cached on `PageMeta` at parse time

### added
- `?` key opens a help overlay showing all keybindings for the current mode; dismiss with `Esc` or `?`
- PageUp/PageDown keys in page mode scroll by half a screen

### fixed
- plugin request timeout (`LEPITER_PLUGIN_TIMEOUT_MS`) is now enforced: a hung plugin no longer
  freezes the TUI indefinitely. Timed-out plugins are killed and respawned automatically before
  retrying.
- pressing `G` (jump to bottom) then `k`/Up no longer leaves the user stuck; `page_scroll`
  is clamped to content length before applying scroll deltas
- search snippets and match highlighting no longer silently break for pages with non-ASCII
  content (byte offsets from lowercased text are now correctly mapped back to the raw text)
- pressing Esc in search mode now clears the search and restores the full page list
- `page:Title` links now fall back to title resolution when the target is not a page ID
- `G` (jump to bottom) no longer wraps `page_scroll` on `u16` cast, fixing garbled scroll position
- plugin cache now uses LRU eviction instead of arbitrary hash-order eviction
- plugin IPC no longer spin-waits at 100% CPU when a plugin process crashes mid-request
- `LEPITER_PLUGIN_CACHE=0` now correctly disables the plugin cache even when the config file
  is missing or contains invalid JSON (was silently clamped to 1)
- code highlighting now correctly handles escaped backslashes in string literals
  (e.g. `"path\\to\\file"` no longer bleeds color past the closing quote)

## 0.6.0 - 2026-03-03

### added
- tui edit mode for text/code snippets with autosave and undo
- inline edit view: selected snippet highlighted in-page with a cursor marker
- edit-time syntax highlighting for code blocks
- annotation highlighting in text (`{{annotation:...}}`)
- editor documentation (`docs/editor.md`)

### changed
- non-editable snippets are tinted with a light yellow background in edit view
- read-only snippets are explicitly non-editable in the editor

### fixed
- multiline cursor tracking now handles `\\r` line breaks correctly

## 0.5.0 - 2026-03-01

### added
- `AttachmentResolver` for attachment path resolution and missing-file reporting
- external snippet renderers via ipc (`LEPITER_PLUGIN_CONFIG`)
- plugin sdk types + `lepiter_plugin_main!` macro
- tui snippet editor for text/code with auto-save and undo
- tui external snippet renderer hook via `LEPITER_PLUGIN_CONFIG` (experimental)

## 0.4.0 - 2026-02-28

### added
- bounded tui caches for large knowledge bases:
  - parsed page lru cache
  - rendered page lru cache
  - env configuration:
    - `LEPITER_TUI_PARSED_CACHE` (default `128`)
    - `LEPITER_TUI_RENDERED_CACHE` (default `128`)

### changed
- tui search now behaves as always-on full-text:
  - title/id/tags + content matches
  - incremental background indexing during typing and idle frames
  - content-hit snippet previews in list results
  - opening a content-hit result jumps to the first visible match when possible
- tui list footer now shows full-text index progress and cache occupancy

## 0.3.0 - 2026-02-26

### added
- shared search/title-resolution api in `lepiter-core`:
  - `filter_page_ids`
  - `search_hits`
  - `resolve_page_id_by_title`
  - `SearchMatchKind`, `SearchHit`, `TitleResolution`
- probe unknown-type introspection output (`unknown node types` section)
- probe markdown matrix output mode: `--matrix-md`
- snippet matrix refresh script: `scripts/refresh_snippet_matrix.sh`
- ci snippet-matrix drift check job
- manual snippet-matrix refresh workflow (`workflow_dispatch`)
- snippet parsing support:
  - `pictureSnippet` -> `Node::Link` (media url + caption-aware label)
  - `youtubeSnippet` -> `Node::Link` (`youtubeUrl`)
  - `elementSnippet` -> `Node::Code` (best-effort `code` extraction)
  - `pharoRewrite` -> `Node::Rewrite` (search/replace diff-style block)
  - `wordSnippet` -> `Node::Paragraph` (deterministic text extraction)
- fixture corpus page covering media/element snippets:
  - `lepiter-core/tests/fixtures/corpus/page-media.lepiter`
  - `lepiter-core/tests/fixtures/corpus/page-rewrite.lepiter`
  - `lepiter-core/tests/fixtures/corpus/page-word.lepiter`
- generic open plumbing:
  - target classification api in core (`LinkTargetKind`, `classify_link_target`)
  - tui link-follow can open external urls and attachments using `open` crate
  - `show --open-links` prints resolved link kinds for page links

### changed
- tui/cli search and title resolution now consume shared `lepiter-core` logic
- `docs/snippet-support-matrix.md` is now generated from probe output
- local `scripts/ci.sh` now validates snippet matrix docs are up to date

## 0.2.0 - 2026-02-26

### added
- cli subcommands: `show`, `list`, `ids`, `search`
- `show` defaults to title lookup; `--id` enables explicit id/uuid lookup
- `list` and `search` pretty table output, with `--tsv` for script use
- ansi markdown rendering in `show` with inline emphasis and code highlighting
- checked-in fixture corpus under `lepiter-core/tests/fixtures/corpus`
- ci unsafe gate via `unsafe-budget`
- release workflow with multi-platform binary packaging, sbom, and publish steps

### changed
- windows release packaging now uses `zip` (`Compress-Archive`) instead of `tar`
- parser list handling avoids duplicate list-item rendering in plain output

### fixed
- fixture tests no longer depend on large untracked corpus data
- release msvc packaging path handling failure
