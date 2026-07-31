# editor

the tui editor is snippet-level and read-only for unknown snippet types. it edits only text and code snippets and writes changes back to the page json file.

## editable snippets

- `textSnippet` (`string`, `text`, or `content` field)
- `pharoSnippet`
- `pythonSnippet`
- `javascriptSnippet`
- `shellCommandSnippet`
- `gemstoneSnippet`

## behavior

- open from page view with `e`
- navigate snippets with `tab` / `shift+tab`
- cursor movement with arrows, `home`, `end`
- append a new text snippet with `ctrl+a`
- undo with `ctrl+u` (snapshot-based)
- exit with `esc`; pending changes are saved
- autosave after idle (default `500ms`, `LEPITER_EDIT_AUTOSAVE_MS`)
- non-editable snippets are tinted light yellow
- current snippet is highlighted in-page with a cursor marker

## limitations

- unknown or non-textual snippets are read-only
- no multi-snippet editing in a single buffer
