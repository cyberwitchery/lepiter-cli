# changelog

all notable changes to this project are documented in this file.

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
