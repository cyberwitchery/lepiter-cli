//! `export` → `import` fidelity: every page's node tree must survive the trip.

use std::path::{Path, PathBuf};
use std::process::Command;

use lepiter_core::{KnowledgeBase, Node};
use serde_json::{Value, json};
use tempfile::TempDir;

fn bin_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_lepiter-cli"))
}

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("lepiter-core")
        .join("tests")
        .join("fixtures")
        .join("corpus")
}

fn run(args: &[&Path]) {
    let output = Command::new(bin_path())
        .args(args)
        .output()
        .expect("failed to execute lepiter-cli");
    assert!(
        output.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// a knowledge base holding one page built from `items`.
fn kb_with_snippets(items: Value) -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let page = json!({
        "uid": { "uuid": "roundtrip-page" },
        "pageType": { "title": "roundtrip page" },
        "children": { "items": items }
    });
    std::fs::write(
        dir.path().join("roundtrip-page.lepiter"),
        serde_json::to_vec_pretty(&page).unwrap(),
    )
    .unwrap();
    dir
}

/// node trees as JSON, minus the `raw` payload of an `Unknown` node.
fn normalized_nodes(nodes: &[Node]) -> Value {
    let mut value = serde_json::to_value(nodes).unwrap();
    strip_unknown_raw(&mut value);
    value
}

fn strip_unknown_raw(value: &mut Value) {
    match value {
        Value::Array(items) => items.iter_mut().for_each(strip_unknown_raw),
        Value::Object(obj) => {
            if obj.get("type").and_then(Value::as_str) == Some("unknown") {
                obj.remove("raw");
            }
            obj.values_mut().for_each(strip_unknown_raw);
        }
        _ => {}
    }
}

/// exports `kb`, imports it back, and asserts every page's node tree is
/// unchanged.
fn assert_roundtrips(kb: &Path) {
    let work = tempfile::tempdir().expect("tempdir");
    let markdown = work.path().join("markdown");
    let reimported = work.path().join("reimported");

    run(&[Path::new("export"), &markdown, kb]);
    run(&[Path::new("import"), &markdown, &reimported]);

    let before = KnowledgeBase::open(kb).expect("open source kb");
    let after = KnowledgeBase::open(&reimported).expect("open reimported kb");

    let ids: Vec<String> = before.sorted_pages().iter().map(|m| m.id.clone()).collect();
    assert!(
        !ids.is_empty(),
        "no pages to round-trip in {}",
        kb.display()
    );

    for id in ids {
        let source = before.load_page(&id).expect("load source page");
        let round_tripped = after
            .load_page(&id)
            .unwrap_or_else(|e| panic!("page {id} is missing after the round trip: {e:#}"));
        assert_eq!(
            serde_json::to_string_pretty(&normalized_nodes(&source.content)).unwrap(),
            serde_json::to_string_pretty(&normalized_nodes(&round_tripped.content)).unwrap(),
            "page {id} did not survive export -> import"
        );
    }
}

#[test]
fn corpus_pages_survive_the_roundtrip() {
    assert_roundtrips(&corpus_dir());
}

#[test]
fn code_snippet_containing_a_fence_survives() {
    let kb = kb_with_snippets(json!([{
        "__type": "pythonSnippet",
        "code": "doc = '''\n```\nnested fence\n```\n'''\nprint(doc)",
    }]));
    assert_roundtrips(kb.path());
}

#[test]
fn multi_line_text_snippet_stays_one_snippet() {
    let kb = kb_with_snippets(json!([
        { "__type": "textSnippet", "string": "para line one\npara line two" },
    ]));
    assert_roundtrips(kb.path());
}

#[test]
fn multi_line_heading_stays_one_snippet() {
    let kb = kb_with_snippets(json!([
        { "__type": "textSnippet", "string": "# heading\ntrailing prose" },
    ]));
    assert_roundtrips(kb.path());
}

#[test]
fn multi_line_quote_stays_one_snippet() {
    let kb = kb_with_snippets(json!([
        { "__type": "textSnippet", "string": "> quote line one\nquote line two" },
    ]));
    assert_roundtrips(kb.path());
}

#[test]
fn prose_that_looks_like_a_list_stays_prose() {
    let kb = kb_with_snippets(json!([
        { "__type": "textSnippet", "string": "- looks like a list but is prose" },
    ]));
    assert_roundtrips(kb.path());
}

#[test]
fn prose_that_looks_like_an_unknown_marker_stays_prose() {
    let kb = kb_with_snippets(json!([
        { "__type": "textSnippet", "string": "[[unknown: notReallyASnippet]]" },
    ]));
    assert_roundtrips(kb.path());
}

#[test]
fn text_snippet_with_a_blank_line_stays_one_snippet() {
    let kb = kb_with_snippets(json!([
        { "__type": "textSnippet", "string": "para one\n\npara two" },
    ]));
    assert_roundtrips(kb.path());
}

#[test]
fn text_snippet_keeps_its_leading_and_trailing_blank_lines() {
    let kb = kb_with_snippets(json!([
        { "__type": "textSnippet", "string": "\nbody\n" },
    ]));
    assert_roundtrips(kb.path());
}

#[test]
fn blank_text_snippets_survive_next_to_their_neighbours() {
    let kb = kb_with_snippets(json!([
        { "__type": "textSnippet", "string": "before" },
        { "__type": "textSnippet", "string": "" },
        { "__type": "textSnippet", "string": "   " },
        { "__type": "textSnippet", "string": "after" },
    ]));
    assert_roundtrips(kb.path());
}

#[test]
fn prose_lines_after_the_first_that_look_like_blocks_stay_prose() {
    let kb = kb_with_snippets(json!([
        { "__type": "textSnippet", "string": "intro\n- not a list\n> not a quote\n```\nnot code\n```" },
    ]));
    assert_roundtrips(kb.path());
}

// guards

#[test]
fn guard_code_snippet_containing_a_longer_fence_survives() {
    let kb = kb_with_snippets(json!([{
        "__type": "jsonSnippet",
        "code": "outer\n````\ninner\n````\ndone",
    }]));
    assert_roundtrips(kb.path());
}

#[test]
fn guard_prose_that_looks_like_a_fence_stays_prose() {
    let kb = kb_with_snippets(json!([
        { "__type": "textSnippet", "string": "```rust\nnot really a code snippet\n```" },
    ]));
    assert_roundtrips(kb.path());
}

#[test]
fn guard_prose_that_starts_with_a_backslash_survives() {
    let kb = kb_with_snippets(json!([
        { "__type": "textSnippet", "string": "\\- already escaped by hand" },
        { "__type": "textSnippet", "string": "\\newcommand{\\foo}{bar}" },
    ]));
    assert_roundtrips(kb.path());
}

#[test]
fn guard_prose_that_is_exactly_a_link_stays_prose() {
    let kb = kb_with_snippets(json!([
        { "__type": "textSnippet", "string": "[label](https://example.com)" },
        { "__type": "textSnippet", "string": "  [padded](https://example.com)  " },
    ]));
    assert_roundtrips(kb.path());
}

#[test]
fn guard_list_item_that_is_a_link_stays_a_list_item() {
    let kb = kb_with_snippets(json!([
        {
            "__type": "listSnippet",
            "children": { "items": [
                { "__type": "textSnippet", "string": "[label](https://example.com)" },
                { "__type": "textSnippet", "string": "plain item" },
            ] }
        },
    ]));
    assert_roundtrips(kb.path());
}

#[test]
fn guard_link_snippet_still_comes_back_as_a_link() {
    let kb = kb_with_snippets(json!([
        { "__type": "linkSnippet", "string": "label", "url": "https://example.com" },
    ]));
    assert_roundtrips(kb.path());
}

#[test]
fn guard_prose_with_a_link_line_among_others_stays_prose() {
    let kb = kb_with_snippets(json!([
        { "__type": "textSnippet", "string": "intro\n[label](https://example.com)\noutro" },
    ]));
    assert_roundtrips(kb.path());
}

#[test]
fn guard_rewrite_block_survives_alongside_a_code_snippet() {
    let kb = kb_with_snippets(json!([
        {
            "__type": "pharoRewrite",
            "search": "children isNil ifTrue: [ ^ self ].",
            "replace": "children ifNil: [ ^ self ].",
            "isMethodPattern": false,
            "scope": "#(#('searchMethods' '') )",
        },
        { "__type": "pythonSnippet", "code": "print('after the rewrite')" },
    ]));
    assert_roundtrips(kb.path());
}

#[test]
fn guard_adjacent_text_snippets_stay_separate() {
    let kb = kb_with_snippets(json!([
        { "__type": "textSnippet", "string": "first snippet" },
        { "__type": "textSnippet", "string": "second snippet" },
        { "__type": "textSnippet", "string": "third snippet" },
    ]));
    assert_roundtrips(kb.path());
}

#[test]
fn guard_list_followed_by_prose_stays_split() {
    let kb = kb_with_snippets(json!([
        {
            "__type": "listSnippet",
            "children": { "items": [
                { "__type": "textSnippet", "string": "first item" },
                { "__type": "textSnippet", "string": "second item" },
            ] }
        },
        { "__type": "textSnippet", "string": "prose after the list" },
    ]));
    assert_roundtrips(kb.path());
}
