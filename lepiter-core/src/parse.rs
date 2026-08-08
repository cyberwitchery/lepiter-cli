use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::DateTime;
use serde::Deserialize;
use serde_json::Value;

use crate::model::{Node, PageMeta};

/// maximum number of lines kept in a word-snippet paragraph
const MAX_WORD_SNIPPET_LINES: usize = 8;

/// Maximum number of characters in a word-snippet paragraph before it is
/// truncated with an ellipsis.
const MAX_WORD_SNIPPET_CHARS: usize = 1200;

#[derive(Debug, Deserialize)]
struct RawMeta {
    #[serde(default)]
    uid: Option<RawUid>,
    #[serde(default)]
    #[serde(rename = "pageType")]
    page_type: Option<RawPageType>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    #[serde(rename = "editTime")]
    edit_time: Option<RawEditTime>,
    #[serde(default)]
    tags: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct RawUid {
    #[serde(default)]
    uuid: Option<String>,
    #[serde(default)]
    #[serde(rename = "uidString")]
    uid_string: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawPageType {
    #[serde(default)]
    title: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawEditTime {
    #[serde(default)]
    time: Option<RawTimeValue>,
}

#[derive(Debug, Deserialize)]
struct RawTimeValue {
    #[serde(default)]
    #[serde(rename = "dateAndTimeString")]
    date_and_time_string: Option<String>,
}

/// Parses a single raw snippet JSON value into a [`Node`].
///
/// This is a best-effort conversion that preserves unknown snippet types as
/// [`Node::Unknown`]. It is intended for tooling that operates on raw JSON
/// without loading a full page.
pub fn parse_node_from_raw(item: &Value) -> Node {
    parse_node(item)
}

pub(crate) fn parse_page_meta(path: &Path) -> Result<PageMeta> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let raw: RawMeta =
        serde_json::from_reader(reader).with_context(|| "failed to decode page metadata")?;

    let id = raw
        .uid
        .as_ref()
        .and_then(|u| u.uuid.clone().or_else(|| u.uid_string.clone()))
        .unwrap_or_default();
    let title = raw
        .page_type
        .and_then(|pt| pt.title)
        .or(raw.title)
        .unwrap_or_default();
    let updated_at = raw
        .edit_time
        .and_then(|e| e.time)
        .and_then(|t| t.date_and_time_string)
        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok());
    let tags = parse_tags(raw.tags.as_ref());

    let id_lower = id.to_lowercase();
    let title_lower = title.to_lowercase();
    let tags_lower = tags.iter().map(|t| t.to_lowercase()).collect();
    Ok(PageMeta {
        id,
        id_lower,
        title,
        title_lower,
        path: path.to_path_buf(),
        updated_at,
        tags,
        tags_lower,
    })
}

fn parse_tags(value: Option<&Value>) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    match value {
        Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                item.as_str()
                    .map(ToOwned::to_owned)
                    .or_else(|| {
                        item.get("name")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned)
                    })
                    .or_else(|| {
                        item.get("title")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned)
                    })
            })
            .collect(),
        Value::Object(obj) => obj
            .get("items")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|i| {
                        i.get("title")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

pub(crate) fn parse_item_recursive(item: &Value, out: &mut Vec<Node>) {
    let typ = extract_type(item);
    out.push(parse_node(item));
    if matches!(typ, Some("listSnippet")) {
        // list snippets already materialize children into Node::List items.
        return;
    }
    if let Some(children) = item
        .get("children")
        .and_then(|v| v.get("items"))
        .and_then(Value::as_array)
    {
        for child in children {
            parse_item_recursive(child, out);
        }
    }
}

fn parse_node(item: &Value) -> Node {
    let typ = extract_type(item);

    match typ {
        Some("textSnippet") => parse_text_like_node(item),
        Some("quoteSnippet") | Some("blockQuoteSnippet") | Some("commentSnippet") => Node::Quote {
            text: extract_text(item).unwrap_or_default(),
        },
        Some("listSnippet") => parse_list_node(item),
        Some("pictureSnippet") => parse_picture_node(item),
        Some("youtubeSnippet") => parse_youtube_node(item),
        Some("elementSnippet") => parse_element_node(item),
        Some("pharoRewrite") => parse_rewrite_node(item),
        Some("wordSnippet") => parse_word_node(item),
        Some(t) if is_code_snippet(t) => Node::Code {
            language: infer_language(Some(t)),
            code: extract_code(item)
                .or_else(|| extract_text(item))
                .unwrap_or_default(),
        },
        Some(t @ "pharoLinkSnippet") if has_link(item) => Node::Link {
            text: extract_text(item).unwrap_or_else(|| t.to_string()),
            url: extract_link(item).unwrap_or_default(),
        },
        Some("linkSnippet") if has_link(item) => Node::Link {
            text: extract_text(item).unwrap_or_else(|| "link".to_string()),
            url: extract_link(item).unwrap_or_default(),
        },
        Some(t) => Node::Unknown {
            typ: t.to_string(),
            raw: item.clone(),
        },
        None => Node::Unknown {
            typ: "<missing-type>".to_string(),
            raw: item.clone(),
        },
    }
}

fn parse_text_like_node(item: &Value) -> Node {
    let text = extract_text(item).unwrap_or_default();
    if let Some((level, heading)) = parse_heading(&text) {
        Node::Heading {
            level,
            text: heading,
        }
    } else if let Some(stripped) = text.strip_prefix("> ") {
        Node::Quote {
            text: stripped.to_string(),
        }
    } else if text.trim().is_empty() {
        Node::Text { text }
    } else {
        Node::Paragraph { text }
    }
}

fn parse_list_node(item: &Value) -> Node {
    let mut items = Vec::new();
    if let Some(children) = item
        .get("children")
        .and_then(|v| v.get("items"))
        .and_then(Value::as_array)
    {
        for child in children {
            // Promote each item's own children (sub-bullets / continuation
            // blocks) the same way page-level parsing does.
            let mut item_nodes = Vec::new();
            parse_item_recursive(child, &mut item_nodes);
            items.push(item_nodes);
        }
    }
    Node::List { items }
}

fn parse_picture_node(item: &Value) -> Node {
    let url = item
        .get("url")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| extract_link(item))
        .unwrap_or_default();
    let text = item
        .get("caption")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| extract_text(item))
        .unwrap_or_else(|| "picture".to_string());

    if url.is_empty() {
        Node::Unknown {
            typ: "pictureSnippet".to_string(),
            raw: item.clone(),
        }
    } else {
        Node::Link { text, url }
    }
}

fn parse_youtube_node(item: &Value) -> Node {
    let url = item
        .get("youtubeUrl")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| extract_link(item))
        .unwrap_or_default();
    let text = extract_text(item).unwrap_or_else(|| "youtube".to_string());

    if url.is_empty() {
        Node::Unknown {
            typ: "youtubeSnippet".to_string(),
            raw: item.clone(),
        }
    } else {
        Node::Link { text, url }
    }
}

fn parse_element_node(item: &Value) -> Node {
    let code = extract_code(item).or_else(|| extract_text(item));
    if let Some(code) = code.filter(|c| !c.trim().is_empty()) {
        Node::Code {
            language: Some("element".to_string()),
            code,
        }
    } else {
        Node::Unknown {
            typ: "elementSnippet".to_string(),
            raw: item.clone(),
        }
    }
}

fn parse_rewrite_node(item: &Value) -> Node {
    let search = item
        .get("search")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_default();
    let replace = item
        .get("replace")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_default();
    let scope = item
        .get("scope")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let is_method_pattern = item.get("isMethodPattern").and_then(Value::as_bool);

    if search.is_empty() && replace.is_empty() {
        Node::Unknown {
            typ: "pharoRewrite".to_string(),
            raw: item.clone(),
        }
    } else {
        Node::Rewrite {
            language: Some("pharo".to_string()),
            search,
            replace,
            scope,
            is_method_pattern,
        }
    }
}

fn parse_word_node(item: &Value) -> Node {
    let mut lines = Vec::new();

    if let Some(word) = item
        .get("wordString")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        lines.push(word.to_string());
    }

    if let Some(explanation) = item
        .get("explanationAttachmentNameString")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        lines.push(format!("explanation: {explanation}"));
    }

    if lines.is_empty() {
        let mut chars_left = MAX_WORD_SNIPPET_CHARS;
        collect_text_fragments(item, &mut lines, 0, MAX_WORD_SNIPPET_LINES, &mut chars_left);
    }

    lines.retain(|s| !s.trim().is_empty());
    lines.truncate(MAX_WORD_SNIPPET_LINES);

    if lines.is_empty() {
        return Node::Unknown {
            typ: "wordSnippet".to_string(),
            raw: item.clone(),
        };
    }

    let mut text = lines.join("\n");
    let mut char_iter = text.char_indices();
    if let Some((trunc_at, _)) = char_iter.nth(MAX_WORD_SNIPPET_CHARS - 1)
        && char_iter.next().is_some()
    {
        text.truncate(trunc_at);
        text.push('…');
    }

    Node::Paragraph { text }
}

fn collect_text_fragments(
    value: &Value,
    out: &mut Vec<String>,
    depth: usize,
    max_lines: usize,
    chars_left: &mut usize,
) {
    if *chars_left == 0 || out.len() >= max_lines || depth > 4 {
        return;
    }

    match value {
        Value::String(s) => {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                out.push(trimmed.to_string());
                *chars_left = chars_left.saturating_sub(trimmed.len());
            }
        }
        Value::Array(items) => {
            for item in items {
                if out.len() >= max_lines || *chars_left == 0 {
                    break;
                }
                collect_text_fragments(item, out, depth + 1, max_lines, chars_left);
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                if matches!(
                    key.as_str(),
                    "__type"
                        | "children"
                        | "uid"
                        | "createEmail"
                        | "createTime"
                        | "editEmail"
                        | "editTime"
                        | "paragraphStyle"
                ) {
                    continue;
                }
                if out.len() >= max_lines || *chars_left == 0 {
                    break;
                }
                collect_text_fragments(item, out, depth + 1, max_lines, chars_left);
            }
        }
        _ => {}
    }
}

/// Parses a markdown-style heading line, returning the level (1–6) and text.
pub fn parse_heading(input: &str) -> Option<(u8, String)> {
    let trimmed = input.trim();
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if hashes == 0 {
        return None;
    }
    let rest = trimmed[hashes..].trim_start();
    if rest.is_empty() {
        return None;
    }
    Some((hashes.min(6) as u8, rest.to_string()))
}

/// Extracts the snippet type from a JSON value, checking `"type"` then `"__type"`.
pub fn extract_type(item: &Value) -> Option<&str> {
    item.get("type")
        .and_then(Value::as_str)
        .or_else(|| item.get("__type").and_then(Value::as_str))
}

/// Canonical snippet `__type` ↔ code-fence language mapping. Single source of
/// truth behind [`is_code_snippet`], [`infer_language`], and
/// [`language_to_snippet_type`]; each language equals what `infer_language`
/// derives for the type, so export→import round-trips. Languages must stay
/// unique so the reverse lookup is unambiguous.
const CODE_SNIPPET_LANGUAGES: &[(&str, &str)] = &[
    ("pharoSnippet", "pharo"),
    ("pythonSnippet", "python"),
    ("javascriptSnippet", "javascript"),
    ("jsonSnippet", "json"),
    ("yamlSnippet", "yaml"),
    ("shellCommandSnippet", "shellcommand"),
    ("gemstoneSnippet", "gemstone"),
    ("exampleSnippet", "example"),
    ("changesSnippet", "changes"),
    ("robocoderMetamodelSnippet", "robocodermetamodel"),
];

/// Returns `true` if the given snippet type name is a code snippet.
pub fn is_code_snippet(typ: &str) -> bool {
    CODE_SNIPPET_LANGUAGES.iter().any(|(t, _)| *t == typ)
}

fn extract_text(item: &Value) -> Option<String> {
    item.get("string")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            item.get("text")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            item.get("content")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
}

fn extract_code(item: &Value) -> Option<String> {
    item.get("code")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            item.get("source")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
}

fn extract_link(item: &Value) -> Option<String> {
    item.get("url")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            item.get("href")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
}

fn has_link(item: &Value) -> bool {
    item.get("url").and_then(Value::as_str).is_some()
        || item.get("href").and_then(Value::as_str).is_some()
}

fn infer_language(typ: Option<&str>) -> Option<String> {
    let typ = typ?;
    if let Some((_, lang)) = CODE_SNIPPET_LANGUAGES.iter().find(|(t, _)| *t == typ) {
        return Some((*lang).to_string());
    }
    if typ.ends_with("Snippet") {
        Some(typ.trim_end_matches("Snippet").to_lowercase())
    } else {
        None
    }
}

/// Maps a code-fence language back to its snippet `__type`, inverting
/// `infer_language` over the canonical table. Returns `None` when no code
/// snippet type produces that language.
pub fn language_to_snippet_type(lang: &str) -> Option<&'static str> {
    CODE_SNIPPET_LANGUAGES
        .iter()
        .find(|(_, l)| *l == lang)
        .map(|(t, _)| *t)
}

/// Collects all observed `type`/`__type` values and their counts in one page file.
pub fn collect_node_types_in_file(path: &Path) -> Result<HashMap<String, usize>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let raw: Value = serde_json::from_reader(reader).with_context(|| "failed to decode JSON")?;

    let mut out = HashMap::new();
    collect_node_types_value(&raw, &mut out);
    Ok(out)
}

fn collect_node_types_value(value: &Value, out: &mut HashMap<String, usize>) {
    match value {
        Value::Object(map) => {
            if let Some(typ) = extract_type(value) {
                *out.entry(typ.to_string()).or_insert(0) += 1;
            }
            for v in map.values() {
                collect_node_types_value(v, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_node_types_value(item, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;

    /// a file path in a directory no other test shares. `tempfile` picks the
    /// directory name: a timestamp cannot, because `SystemTime::now` ticks at
    /// microsecond granularity here, so two tests in the same process stamp the
    /// same value and collide.
    fn temp_file_path(name: &str) -> PathBuf {
        tempfile::Builder::new()
            .prefix("lepiter-core-")
            .tempdir()
            .expect("temp dir")
            .keep()
            .join(format!("{name}.lepiter"))
    }

    #[test]
    fn parse_heading_detects_markdown_style() {
        assert_eq!(
            parse_heading("## Heading"),
            Some((2, "Heading".to_string()))
        );
        assert_eq!(parse_heading("No heading"), None);
    }

    #[test]
    fn parse_tags_supports_array_and_object_items() {
        let arr = json!(["a", {"name": "b"}, {"title": "c"}]);
        assert_eq!(parse_tags(Some(&arr)), vec!["a", "b", "c"]);

        let obj = json!({"items": [{"title":"x"}, {"title":"y"}]});
        assert_eq!(parse_tags(Some(&obj)), vec!["x", "y"]);
    }

    #[test]
    fn parse_node_covers_known_and_unknown_types() {
        let heading = json!({"__type":"textSnippet","string":"# Title"});
        assert!(matches!(parse_node(&heading), Node::Heading { .. }));

        let quote = json!({"__type":"blockQuoteSnippet","string":"quoted"});
        assert!(matches!(parse_node(&quote), Node::Quote { .. }));

        let code = json!({"__type":"pythonSnippet","code":"print(1)"});
        assert!(matches!(parse_node(&code), Node::Code { .. }));

        let json_code = json!({"__type":"jsonSnippet","code":"{\"key\": 1}"});
        assert!(matches!(parse_node(&json_code), Node::Code { .. }));

        let yaml_code = json!({"__type":"yamlSnippet","code":"key: value"});
        assert!(matches!(parse_node(&yaml_code), Node::Code { .. }));

        let link = json!({"__type":"pharoLinkSnippet","string":"link","url":"page:abc"});
        assert!(matches!(parse_node(&link), Node::Link { .. }));

        let picture = json!({"__type":"pictureSnippet","url":"attachments/x.png","caption":"img"});
        assert!(matches!(parse_node(&picture), Node::Link { .. }));

        let youtube = json!({"__type":"youtubeSnippet","youtubeUrl":"https://youtu.be/abc"});
        assert!(matches!(parse_node(&youtube), Node::Link { .. }));

        let element = json!({"__type":"elementSnippet","code":"GtInspector newOn: 42"});
        assert!(matches!(parse_node(&element), Node::Code { .. }));

        let rewrite =
            json!({"__type":"pharoRewrite","search":"a","replace":"b","isMethodPattern":true});
        assert!(matches!(parse_node(&rewrite), Node::Rewrite { .. }));

        let word = json!({"__type":"wordSnippet","wordString":"refactoring"});
        assert!(matches!(parse_node(&word), Node::Paragraph { .. }));

        let list = json!({
            "__type":"listSnippet",
            "children":{"items":[{"__type":"textSnippet","string":"item"}]}
        });
        assert!(matches!(parse_node(&list), Node::List { .. }));

        let unknown = json!({"__type":"mysterySnippet","x":1});
        assert!(matches!(parse_node(&unknown), Node::Unknown { .. }));

        let missing = json!({"x":1});
        assert!(matches!(parse_node(&missing), Node::Unknown { .. }));
    }

    #[test]
    fn infer_language_maps_common_snippet_types() {
        assert_eq!(
            infer_language(Some("pharoSnippet")),
            Some("pharo".to_string())
        );
        assert_eq!(
            infer_language(Some("javascriptSnippet")),
            Some("javascript".to_string())
        );
        assert_eq!(
            infer_language(Some("jsonSnippet")),
            Some("json".to_string())
        );
        assert_eq!(
            infer_language(Some("yamlSnippet")),
            Some("yaml".to_string())
        );
        assert_eq!(
            infer_language(Some("customSnippet")),
            Some("custom".to_string())
        );
        assert_eq!(infer_language(None), None);
    }

    #[test]
    fn code_snippet_table_round_trips() {
        for &(typ, lang) in CODE_SNIPPET_LANGUAGES {
            assert!(is_code_snippet(typ), "{typ} should be a code snippet");
            assert_eq!(
                infer_language(Some(typ)).as_deref(),
                Some(lang),
                "{typ} infers wrong language"
            );
            assert_eq!(
                language_to_snippet_type(lang),
                Some(typ),
                "{lang} maps back to wrong type"
            );
        }
        // unknown `*Snippet` types still infer via the strip-suffix fallback.
        assert_eq!(infer_language(Some("fooSnippet")).as_deref(), Some("foo"));
        assert_eq!(language_to_snippet_type("foo"), None);
        assert!(!is_code_snippet("textSnippet"));
    }

    #[test]
    fn parse_item_recursive_includes_children() {
        let root = json!({
            "__type":"textSnippet",
            "string":"parent",
            "children":{"items":[
                {"__type":"textSnippet","string":"child"}
            ]}
        });
        let mut out = Vec::new();
        parse_item_recursive(&root, &mut out);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn parse_list_node_promotes_nested_item_children() {
        // A list whose single item (a textSnippet) has its own sub-bullet child.
        let list = json!({
            "__type": "listSnippet",
            "children": {"items": [
                {
                    "__type": "textSnippet",
                    "string": "parent bullet",
                    "children": {"items": [
                        {"__type": "textSnippet", "string": "nested bullet"}
                    ]}
                }
            ]}
        });
        match parse_node(&list) {
            Node::List { items } => {
                assert_eq!(items.len(), 1, "one top-level item expected");
                let item = &items[0];
                assert!(
                    item.len() > 1,
                    "nested child dropped: item holds {} node(s)",
                    item.len()
                );
                assert!(matches!(&item[0], Node::Paragraph { text } if text == "parent bullet"));
                assert!(
                    item[1..]
                        .iter()
                        .any(|n| matches!(n, Node::Paragraph { text } if text == "nested bullet")),
                    "nested bullet content missing from item"
                );
            }
            other => panic!("expected list, got {other:?}"),
        }
    }

    #[test]
    fn parse_list_node_flat_items_unchanged() {
        // a flat list (childless text snippets) parses to exactly one node per item
        let list = json!({
            "__type": "listSnippet",
            "children": {"items": [
                {"__type": "textSnippet", "string": "first"},
                {"__type": "textSnippet", "string": "second"}
            ]}
        });
        match parse_node(&list) {
            Node::List { items } => {
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].len(), 1, "flat item should hold a single node");
                assert_eq!(items[1].len(), 1, "flat item should hold a single node");
                assert!(matches!(&items[0][0], Node::Paragraph { text } if text == "first"));
                assert!(matches!(&items[1][0], Node::Paragraph { text } if text == "second"));
            }
            other => panic!("expected list, got {other:?}"),
        }
    }

    #[test]
    fn parse_list_node_composes_nested_list_child() {
        // A bullet whose child is itself a listSnippet: the nested list must be
        // promoted as its own Node::List with its items fully parsed, exercising
        // the listSnippet early-return in parse_item_recursive.
        let list = json!({
            "__type": "listSnippet",
            "children": {"items": [
                {
                    "__type": "textSnippet",
                    "string": "outer",
                    "children": {"items": [
                        {
                            "__type": "listSnippet",
                            "children": {"items": [
                                {"__type": "textSnippet", "string": "inner"}
                            ]}
                        }
                    ]}
                }
            ]}
        });
        match parse_node(&list) {
            Node::List { items } => {
                assert_eq!(items.len(), 1);
                let item = &items[0];
                let nested = item
                    .iter()
                    .find_map(|n| match n {
                        Node::List { items } => Some(items),
                        _ => None,
                    })
                    .expect("nested list not promoted");
                assert_eq!(nested.len(), 1);
                assert!(matches!(&nested[0][0], Node::Paragraph { text } if text == "inner"));
            }
            other => panic!("expected list, got {other:?}"),
        }
    }

    #[test]
    fn collect_node_types_counts_nested_values() -> Result<()> {
        let path = temp_file_path("types");
        let content = json!({
            "__type":"page",
            "children":{"__type":"snippets","items":[
                {"__type":"textSnippet","children":{"__type":"snippets","items":[]}},
                {"__type":"pythonSnippet","code":"print(1)"}
            ]}
        });
        fs::write(&path, serde_json::to_vec(&content)?)?;
        let counts = collect_node_types_in_file(&path)?;
        fs::remove_file(&path)?;

        assert_eq!(counts.get("page"), Some(&1));
        assert_eq!(counts.get("textSnippet"), Some(&1));
        assert_eq!(counts.get("pythonSnippet"), Some(&1));
        Ok(())
    }

    #[test]
    fn parse_page_meta_extracts_core_fields() -> Result<()> {
        let path = temp_file_path("meta");
        let content = json!({
            "uid":{"uuid":"id-123"},
            "pageType":{"title":"Title"},
            "editTime":{"time":{"dateAndTimeString":"2024-01-01T00:00:00+00:00"}},
            "tags":["t1","t2"]
        });
        fs::write(&path, serde_json::to_vec(&content)?)?;
        let meta = parse_page_meta(&path)?;
        fs::remove_file(&path)?;

        assert_eq!(meta.id, "id-123");
        assert_eq!(meta.title, "Title");
        assert_eq!(meta.tags, vec!["t1", "t2"]);
        assert!(meta.updated_at.is_some());
        Ok(())
    }

    #[test]
    fn parse_page_meta_missing_uid_uses_empty_id() -> Result<()> {
        let path = temp_file_path("no-uid");
        let content = json!({"pageType": {"title": "Some Title"}});
        fs::write(&path, serde_json::to_vec(&content)?)?;
        let meta = parse_page_meta(&path)?;
        fs::remove_file(&path)?;

        assert!(meta.id.is_empty());
        assert_eq!(meta.title, "Some Title");
        Ok(())
    }

    #[test]
    fn parse_page_meta_missing_page_type_uses_empty_title() -> Result<()> {
        let path = temp_file_path("no-pt");
        let content = json!({"uid": {"uuid": "abc-123"}});
        fs::write(&path, serde_json::to_vec(&content)?)?;
        let meta = parse_page_meta(&path)?;
        fs::remove_file(&path)?;

        assert_eq!(meta.id, "abc-123");
        assert!(meta.title.is_empty());
        Ok(())
    }

    #[test]
    fn parse_page_meta_invalid_date_string_yields_none() -> Result<()> {
        let path = temp_file_path("bad-date");
        let content = json!({
            "uid": {"uuid": "id-1"},
            "editTime": {"time": {"dateAndTimeString": "not-a-date"}}
        });
        fs::write(&path, serde_json::to_vec(&content)?)?;
        let meta = parse_page_meta(&path)?;
        fs::remove_file(&path)?;

        assert!(meta.updated_at.is_none());
        Ok(())
    }

    #[test]
    fn parse_word_node_extracts_primary_fields() {
        let item = json!({
            "__type":"wordSnippet",
            "wordString":"refactoring",
            "explanationAttachmentNameString":"attachments/x/explanation.json"
        });
        let node = parse_node(&item);
        match node {
            Node::Paragraph { text } => {
                assert!(text.contains("refactoring"));
                assert!(text.contains("attachments/x/explanation.json"));
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn collect_text_fragments_stops_at_char_budget() {
        let frag = "x".repeat(100);
        let value = json!(["ignore", frag, frag, frag, frag, frag]);
        let mut out = Vec::new();
        let mut chars_left: usize = 250;
        collect_text_fragments(&value, &mut out, 0, MAX_WORD_SNIPPET_LINES, &mut chars_left);
        // "ignore" (6) + three 100-char fragments: pushed then budget
        // saturates to 0, stopping further collection → exactly 4.
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn collect_text_fragments_stops_at_line_limit() {
        let value = json!(["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"]);
        let mut out = Vec::new();
        let mut chars_left = usize::MAX;
        collect_text_fragments(&value, &mut out, 0, 4, &mut chars_left);
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn word_node_fallback_respects_char_budget() {
        // Build a word snippet without wordString/explanationAttachmentNameString
        // so it falls through to collect_text_fragments, with fragments large
        // enough that the char budget kicks in.
        let big = "a".repeat(500);
        let item = json!({
            "__type": "wordSnippet",
            "field1": big,
            "field2": big,
            "field3": big,
            "field4": big,
        });
        let node = parse_node(&item);
        match node {
            Node::Paragraph { text } => {
                // Final text should be at most MAX_WORD_SNIPPET_CHARS + 1 (ellipsis)
                assert!(
                    text.chars().count() <= MAX_WORD_SNIPPET_CHARS + 1,
                    "text too long: {} chars",
                    text.chars().count()
                );
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }
}
