# tui behavior

## modes

- `List`: full-screen page index
- `Search`: filtered list input state
- `Page`: full-screen page reader

## search

- trigger with `/` from any mode
- always-on full-text matching:
  - title, id, tags
  - page content (incrementally indexed in background)
- content matches show snippet previews in the list
- opening a content-hit result attempts to jump to first visible match
- `esc` returns to list view

## rendering

- markdown-like inline styling in text blocks:
  - `**bold**`, `*italic*`, `` `inline code` ``, links
- basic language-aware code highlighting for common snippet languages
- control characters are sanitized for terminal safety

## link navigation

links are extracted from:

- explicit `Node::Link`
- markdown links `[text](target)`
- wiki-style links `[[Title]]`

navigation:

- `tab` / `shift+tab`: select link
- `enter`: follow selected link
- `h`: back through link history

internal targets resolve by:

- exact page id
- `page:<id>`
- uuid-like target text
- exact page-title match

external targets:

- attachment paths and urls are opened via the system opener (`open` crate)
- failures are reported in the tui status line

## caching

tui keeps bounded lru caches:

- parsed pages
- rendered pages

configure limits with env vars:

- `LEPITER_TUI_PARSED_CACHE` (default `128`)
- `LEPITER_TUI_RENDERED_CACHE` (default `128`)

the list footer shows cache occupancy and full-text index progress.
