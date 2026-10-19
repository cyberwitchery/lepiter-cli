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

/// writes a page built from `items` into the knowledge base at `kb`.
fn write_page(kb: &Path, id: &str, title: &str, items: Value) {
    let page = json!({
        "uid": { "uuid": id },
        "pageType": { "title": title },
        "children": { "items": items }
    });
    std::fs::write(
        kb.join(format!("{id}.lepiter")),
        serde_json::to_vec_pretty(&page).unwrap(),
    )
    .unwrap();
}

/// a knowledge base holding one page built from `items`.
fn kb_with_snippets(items: Value) -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    write_page(dir.path(), "roundtrip-page", "roundtrip page", items);
    dir
}

/// a list snippet with one item per entry of `items`.
fn list(items: Value) -> Value {
    json!({ "__type": "listSnippet", "children": { "items": items } })
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

/// the markdown `export` writes for the page `kb_with_snippets` builds.
fn exported_markdown(kb: &Path) -> String {
    let work = tempfile::tempdir().expect("tempdir");
    let markdown = work.path().join("markdown");
    run(&[Path::new("export"), &markdown, kb]);
    let page = markdown.join("roundtrip-page.md");
    std::fs::read_to_string(&page).unwrap_or_else(|e| panic!("read {}: {e}", page.display()))
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

/// each case is a page of its own (id and title are the case name) in one
/// knowledge base, so one export and one import cover them all;
/// `assert_roundtrips` names the page that failed.
#[test]
fn snippet_cases_survive_the_roundtrip() {
    let cases: Vec<(&str, Value)> = vec![
        (
            "code_snippet_containing_a_fence_survives",
            json!([{
                "__type": "pythonSnippet",
                "code": "doc = '''\n```\nnested fence\n```\n'''\nprint(doc)",
            }]),
        ),
        (
            "multi_line_text_snippet_stays_one_snippet",
            json!([
                { "__type": "textSnippet", "string": "para line one\npara line two" },
            ]),
        ),
        (
            "multi_line_heading_stays_one_snippet",
            json!([
                { "__type": "textSnippet", "string": "# heading\ntrailing prose" },
            ]),
        ),
        (
            "multi_line_quote_stays_one_snippet",
            json!([
                { "__type": "textSnippet", "string": "> quote line one\nquote line two" },
            ]),
        ),
        (
            "prose_that_looks_like_a_list_stays_prose",
            json!([
                { "__type": "textSnippet", "string": "- looks like a list but is prose" },
            ]),
        ),
        (
            "prose_that_looks_like_an_unknown_marker_stays_prose",
            json!([
                { "__type": "textSnippet", "string": "[[unknown: notReallyASnippet]]" },
            ]),
        ),
        (
            "text_snippet_with_a_blank_line_stays_one_snippet",
            json!([
                { "__type": "textSnippet", "string": "para one\n\npara two" },
            ]),
        ),
        (
            "text_snippet_keeps_its_leading_and_trailing_blank_lines",
            json!([
                { "__type": "textSnippet", "string": "\nbody\n" },
            ]),
        ),
        (
            "blank_text_snippets_survive_next_to_their_neighbours",
            json!([
                { "__type": "textSnippet", "string": "before" },
                { "__type": "textSnippet", "string": "" },
                { "__type": "textSnippet", "string": "   " },
                { "__type": "textSnippet", "string": "after" },
            ]),
        ),
        (
            "list_item_paragraph_keeps_its_code_block_child",
            json!([list(json!([{
                "__type": "textSnippet",
                "string": "intro",
                "children": { "items": [
                    { "__type": "pythonSnippet", "code": "print('hi')\n\nprint('bye')" },
                ] }
            }]))]),
        ),
        (
            "list_item_quote_survives",
            json!([list(json!([
                { "__type": "textSnippet", "string": "> first\n\nthird" },
                {
                    "__type": "textSnippet",
                    "string": "intro",
                    "children": { "items": [
                        { "__type": "textSnippet", "string": "> quoted" },
                    ] }
                },
            ]))]),
        ),
        (
            "list_item_nested_list_survives",
            json!([list(json!([
                {
                    "__type": "textSnippet",
                    "string": "outer",
                    "children": { "items": [list(json!([
                        {
                            "__type": "textSnippet",
                            "string": "inner one",
                            "children": { "items": [
                                { "__type": "pythonSnippet", "code": "print('deep')" },
                            ] }
                        },
                        { "__type": "textSnippet", "string": "inner two" },
                    ]))] }
                },
                list(json!([
                    { "__type": "textSnippet", "string": "a" },
                    { "__type": "textSnippet", "string": "b" },
                ])),
            ]))]),
        ),
        (
            "list_item_unknown_snippet_survives_next_to_prose_spelling_its_marker",
            json!([list(json!([
                { "__type": "wardleyMap" },
                { "__type": "textSnippet", "string": "[[unknown: wardleyMap]]" },
            ]))]),
        ),
        (
            "prose_lines_after_the_first_that_look_like_blocks_stay_prose",
            json!([
                { "__type": "textSnippet", "string": "intro\n- not a list\n> not a quote\n```\nnot code\n```" },
            ]),
        ),
        (
            "guard_code_snippet_containing_a_longer_fence_survives",
            json!([{
                "__type": "jsonSnippet",
                "code": "outer\n````\ninner\n````\ndone",
            }]),
        ),
        (
            "guard_prose_that_looks_like_a_fence_stays_prose",
            json!([
                { "__type": "textSnippet", "string": "```rust\nnot really a code snippet\n```" },
            ]),
        ),
        (
            "a_link_in_a_code_block_in_prose_survives",
            json!([
                {
                    "__type": "textSnippet",
                    "string": "```\n[[a_link_in_a_code_block_in_prose_survives]]\n```",
                },
            ]),
        ),
        (
            "guard_prose_that_starts_with_a_backslash_survives",
            json!([
                { "__type": "textSnippet", "string": "\\- already escaped by hand" },
                { "__type": "textSnippet", "string": "\\newcommand{\\foo}{bar}" },
            ]),
        ),
        (
            "guard_link_snippet_still_comes_back_as_a_link",
            json!([
                { "__type": "linkSnippet", "string": "label", "url": "https://example.com" },
            ]),
        ),
        (
            "guard_rewrite_block_survives_alongside_a_code_snippet",
            json!([
                {
                    "__type": "pharoRewrite",
                    "search": "children isNil ifTrue: [ ^ self ].",
                    "replace": "children ifNil: [ ^ self ].",
                    "isMethodPattern": false,
                    "scope": "#(#('searchMethods' '') )",
                },
                { "__type": "pythonSnippet", "code": "print('after the rewrite')" },
            ]),
        ),
        (
            "guard_adjacent_text_snippets_stay_separate",
            json!([
                { "__type": "textSnippet", "string": "first snippet" },
                { "__type": "textSnippet", "string": "second snippet" },
                { "__type": "textSnippet", "string": "third snippet" },
            ]),
        ),
        (
            "guard_list_followed_by_prose_stays_split",
            json!([
                {
                    "__type": "listSnippet",
                    "children": { "items": [
                        { "__type": "textSnippet", "string": "first item" },
                        { "__type": "textSnippet", "string": "second item" },
                    ] }
                },
                { "__type": "textSnippet", "string": "prose after the list" },
            ]),
        ),
    ];
    let kb = tempfile::tempdir().expect("tempdir");
    for (name, items) in cases {
        write_page(kb.path(), name, name, items);
    }
    assert_roundtrips(kb.path());
}

#[test]
fn list_item_code_keeps_its_backslashes_and_page_links() {
    let kb = kb_with_snippets(json!([list(json!([{
        "__type": "pythonSnippet",
        "code": "\\d+ starts this line\nprint('[x](two.md)')",
    }]))]));
    write_page(
        kb.path(),
        "two-page",
        "two",
        json!([{ "__type": "textSnippet", "string": "page two" }]),
    );
    assert_roundtrips(kb.path());
}

#[test]
fn list_item_link_snippet_and_link_only_paragraph_stay_apart() {
    let kb = kb_with_snippets(json!([list(json!([
        { "__type": "linkSnippet", "string": "label", "url": "https://example.com" },
        { "__type": "textSnippet", "string": "[label](https://example.com)" },
    ]))]));
    let markdown = exported_markdown(kb.path());
    assert!(
        markdown.contains("- [label](https://example.com)\n- \\[label](https://example.com)\n"),
        "a link snippet and a link-only paragraph are written alike:\n{markdown}"
    );
    assert_roundtrips(kb.path());
}

#[test]
fn guard_prose_that_is_exactly_a_link_stays_prose() {
    let kb = kb_with_snippets(json!([
        { "__type": "textSnippet", "string": "[label](https://example.com)" },
        { "__type": "textSnippet", "string": "  [padded](https://example.com)  " },
    ]));
    let markdown = exported_markdown(kb.path());
    assert!(
        markdown.contains("\\[label](https://example.com)\n"),
        "a whole-snippet link was not escaped:\n{markdown}"
    );
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
    let markdown = exported_markdown(kb.path());
    assert!(
        markdown.contains("- \\[label](https://example.com)\n"),
        "a list item that is only a link was not escaped:\n{markdown}"
    );
    assert_roundtrips(kb.path());
}

#[test]
fn guard_prose_with_a_link_line_among_others_stays_prose() {
    let kb = kb_with_snippets(json!([
        { "__type": "textSnippet", "string": "intro\n[label](https://example.com)\noutro" },
    ]));
    let markdown = exported_markdown(kb.path());
    assert!(
        markdown.contains("intro\n[label](https://example.com)\noutro\n"),
        "a link line inside a paragraph was escaped into literal text:\n{markdown}"
    );
    assert_roundtrips(kb.path());
}
