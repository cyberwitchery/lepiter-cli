# changelog

all notable changes to this project are documented in this file.

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
