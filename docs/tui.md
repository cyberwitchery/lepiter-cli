# tui behavior

## modes

- `List`: full-screen page index
- `Search`: filtered list input state
- `Page`: full-screen page reader

## search

- trigger with `/` from any mode
- matches title, id, and tags
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
