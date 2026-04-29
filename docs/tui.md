# tui behavior

## modes

- `List`: full-screen page index
- `Search`: filtered list input state
- `Page`: full-screen page reader
- `Edit`: snippet-level editor (text/code only)

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
- unknown snippet types can be rendered by external plugins (see `docs/plugins.md`)

## editing

- open with `e` from page view
- exit with `esc`
- navigate snippets with `tab` / `shift+tab`
- cursor movement with arrow keys, `home`, `end`
- `ctrl+u` undo (snapshot-based)
- auto-save after idle (default 500ms, `LEPITER_EDIT_AUTOSAVE_MS`)
- editable snippets: `textSnippet` and code snippets (`pharoSnippet`, `pythonSnippet`, `javascriptSnippet`, `shellCommandSnippet`, `gemstoneSnippet`)
- non-editable snippets are read-only and tinted light yellow
- the current snippet is highlighted in-page with a visible cursor marker

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

- attachment paths are resolved relative to the kb root before opening
- urls are opened via the system opener (`open` crate)
- failures are reported in the tui status line

## open externally

press `O` in page mode to open the current page with an external command. this
requires the `LEPITER_OPEN_CMD` environment variable to be set to a shell
command. the command receives `LEPITER_PAGE_ID` (page uuid) and
`LEPITER_PAGE_PATH` (absolute path to the `.lepiter` file) as environment
variables.

## caching

tui keeps bounded lru caches:

- parsed pages
- rendered pages

configure limits with env vars:

- `LEPITER_TUI_PARSED_CACHE` (default `128`)
- `LEPITER_TUI_RENDERED_CACHE` (default `128`)
- `LEPITER_OPEN_CMD`: shell command to open a page externally (see "open externally" above)

the list footer shows cache occupancy and full-text index progress.

## keybindings reference

### list / search mode

| key | action |
|-----|--------|
| `↑` / `k` | move selection up |
| `↓` / `j` | move selection down |
| `enter` | open selected page |
| `/` | enter search mode |
| `esc` | clear search / return to list |
| `backspace` | delete last search character |

### page mode

| key | action |
|-----|--------|
| `↑` / `k` | scroll up one line |
| `↓` / `j` | scroll down one line |
| `page up` | scroll up half a screen |
| `page down` | scroll down half a screen |
| `g` | jump to top |
| `G` | jump to bottom |
| `tab` | select next link |
| `shift+tab` | select previous link |
| `enter` | follow selected link |
| `h` | go back in link history |
| `b` | back to list |
| `e` | enter edit mode |
| `O` | open current page externally (`LEPITER_OPEN_CMD`) |
| `esc` | back to list |
| `q` | quit |

### edit mode

| key | action |
|-----|--------|
| `tab` / `shift+tab` | move to next / previous snippet |
| `↑` / `↓` / `←` / `→` | cursor movement |
| `home` / `end` | cursor to line start / end |
| `ctrl+u` | undo (snapshot-based) |
| `backspace` | delete character before cursor |
| `delete` | delete character at cursor |
| `esc` | exit edit mode |
