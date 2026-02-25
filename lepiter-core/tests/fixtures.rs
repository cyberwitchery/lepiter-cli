use std::path::PathBuf;

use anyhow::Result;
use lepiter_core::{KnowledgeBase, Node, render_page_to_text};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("lepiter")
}

#[test]
fn golden_parse_page_has_content() -> Result<()> {
    let index = KnowledgeBase::open(fixtures_dir())?;
    let page = index.load_page("60115ab5-94bf-0d00-957c-482d010a0a62")?;
    assert!(!page.content.is_empty());
    Ok(())
}

#[test]
fn corpus_parse_does_not_panic_and_preserves_unknown() -> Result<()> {
    let index = KnowledgeBase::open(fixtures_dir())?;
    let mut saw_unknown = false;

    for id in index.pages.keys() {
        let page = index.load_page(id)?;
        for node in page.content {
            if matches!(node, Node::Unknown { .. }) {
                saw_unknown = true;
                break;
            }
        }
    }

    assert!(
        saw_unknown,
        "expected to encounter at least one unknown node type"
    );
    Ok(())
}

#[test]
fn render_smoke_test() -> Result<()> {
    let index = KnowledgeBase::open(fixtures_dir())?;
    let page = index.load_page("60115ab5-94bf-0d00-957c-482d010a0a62")?;
    let rendered = render_page_to_text(&page);
    assert!(!rendered.trim().is_empty());
    Ok(())
}
