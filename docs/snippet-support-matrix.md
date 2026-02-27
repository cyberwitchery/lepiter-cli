# snippet support matrix

this matrix is generated from `cargo run -p lepiter-core --example probe -- --matrix-md <kb-path>`.

| source type | observed | parser mapping | render | link nav | status |
|---|---:|---|---|---|---|
| `textSnippet` | 4 | `Node::Paragraph`/`Node::Heading`/`Node::Text` | markdown-like | yes | full |
| `elementSnippet` | 1 | `Node::Code` | code block | no | partial |
| `listSnippet` | 1 | `Node::List` | list block | no | full |
| `mysterySnippet` | 1 | `Node::Unknown` | `[[unknown: <type>]]` | no | fallback |
| `pharoLinkSnippet` | 1 | `Node::Link` | link line | yes | full |
| `pharoRewrite` | 1 | `Node::Rewrite` | rewrite diff block | no | full |
| `pictureSnippet` | 1 | `Node::Link` | link line (media reference) | yes (target-dependent) | partial |
| `pythonSnippet` | 1 | `Node::Code` | highlighted code | no | full |
| `quoteSnippet` | 1 | `Node::Quote` | quote block | no | full |
| `wordSnippet` | 1 | `Node::Paragraph` | paragraph text | no | full |
| `youtubeSnippet` | 1 | `Node::Link` | link line (youtube url) | yes (target-dependent) | partial |
