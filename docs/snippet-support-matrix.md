# snippet support matrix

this matrix tracks snippet-like/source types observed in `./lepiter` and current support in `lepiter-core` + `lepiter-cli`.

legend:

- `full`: parsed to a concrete block node and rendered with dedicated behavior
- `partial`: parsed to a concrete block node but rendered generically
- `fallback`: preserved as `Node::Unknown` and rendered as `[[unknown: <type>]]`

## observed snippet-like types

| source type | observed in corpus | parser mapping | tui render | link nav | status |
|---|---:|---|---|---|---|
| `textSnippet` | 8399 | `Node::Paragraph`/`Node::Heading`/`Node::Text` | markdown-like inline formatting + wiki/markdown link extraction | yes (via extracted links) | full |
| `blockQuoteSnippet` | 44 | `Node::Quote` | styled quote block | no | full |
| `commentSnippet` | 6 | `Node::Quote` | styled quote block | no | full |
| `pharoSnippet` | 1661 | `Node::Code { language: pharo }` | code block + basic pharo keyword highlighting | no | full |
| `pythonSnippet` | 55 | `Node::Code { language: python }` | code block + basic python highlighting | no | full |
| `javascriptSnippet` | 8 | `Node::Code { language: javascript }` | code block + basic javascript highlighting | no | full |
| `shellCommandSnippet` | 14 | `Node::Code { language: shellcommand }` | code block + shell-like highlighting | no | partial |
| `gemstoneSnippet` | 37 | `Node::Code { language: gemstone }` | code block (generic) | no | partial |
| `exampleSnippet` | 465 | `Node::Code { language: example }` | code block (generic) | no | partial |
| `changesSnippet` | 87 | `Node::Code { language: changes }` | code block (generic) | no | partial |
| `robocoderMetamodelSnippet` | 3 | `Node::Code { language: robocodermetamodel }` | code block (generic) | no | partial |
| `pharoLinkSnippet` | 2 | `Node::Link` | link line with numbered target | yes (internal targets) | full |
| `elementSnippet` | 49 | `Node::Unknown` | unknown placeholder | no | fallback |
| `pictureSnippet` | 30 | `Node::Unknown` | unknown placeholder | no | fallback |
| `wordSnippet` | 1 | `Node::Unknown` | unknown placeholder | no | fallback |
| `youtubeSnippet` | 7 | `Node::Unknown` | unknown placeholder | no | fallback |
| `pharoRewrite` | 12 | `Node::Unknown` | unknown placeholder | no | fallback |

## structural/non-snippet types seen in the same files

these are metadata/container types, not user-facing snippet blocks:

- `page`, `namedPage`, `unnamedPage`, `snippets`
- `uuid`, `uid`, `textStyle`
- `time`, `dateAndTime`, `email`
- `gtGemStoneDefaultSessionIdentifier`, `gtGemStoneExplicitSessionIdentifier`
- `GraphQL`, `SmaCCRewrite`, `wardleyMap`

## next expansion candidates

high-value types to map next (in order):

1. `pictureSnippet` -> dedicated media block (show caption/url/path)
2. `youtubeSnippet` -> dedicated video link block (title + url)
3. `elementSnippet` -> inspect key fields and map to code/text/media surrogate
4. `pharoRewrite` -> dedicated code-like rewrite block
5. `wordSnippet` -> text/code hybrid depending on payload fields

## how to refresh this matrix

1. run the probe:

```bash
cargo run -p lepiter-core --example probe -- ./lepiter
```

2. compare `node types observed` with this table and update mapping/status rows.
