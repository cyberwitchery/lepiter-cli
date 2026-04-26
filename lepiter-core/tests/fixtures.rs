use std::path::PathBuf;

use anyhow::Result;
use lepiter_core::{KnowledgeBase, Node, render_page_to_text};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("corpus")
}

fn first_page_id(index: &lepiter_core::KnowledgeBaseIndex) -> Option<String> {
    index.sorted_pages().first().map(|m| m.id.clone())
}

#[test]
fn golden_parse_page_has_content() -> Result<()> {
    let index = KnowledgeBase::open(fixtures_dir())?;
    let Some(id) = first_page_id(&index) else {
        panic!("expected at least one page in checked-in fixture corpus");
    };
    let page = index.load_page(&id)?;
    assert!(!page.content.is_empty());
    Ok(())
}

#[test]
fn corpus_parse_does_not_panic_and_preserves_unknown() -> Result<()> {
    let index = KnowledgeBase::open(fixtures_dir())?;
    let mut saw_unknown = false;
    let mut parsed_pages = 0usize;

    for id in index.pages.keys() {
        let page = index.load_page(id)?;
        parsed_pages += 1;
        for node in page.content {
            if matches!(node, Node::Unknown { .. }) {
                saw_unknown = true;
                break;
            }
        }
    }

    assert!(
        parsed_pages > 0,
        "expected at least one page in fixture corpus"
    );
    assert!(
        saw_unknown,
        "expected checked-in fixtures to include unknown node types"
    );
    Ok(())
}

#[test]
fn render_smoke_test() -> Result<()> {
    let index = KnowledgeBase::open(fixtures_dir())?;
    let Some(id) = first_page_id(&index) else {
        panic!("expected at least one page in checked-in fixture corpus");
    };
    let page = index.load_page(&id)?;
    let rendered = render_page_to_text(&page);
    assert!(!rendered.trim().is_empty());
    Ok(())
}
