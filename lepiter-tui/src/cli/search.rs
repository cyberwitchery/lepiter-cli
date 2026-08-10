use std::collections::HashMap;
use std::io::Write;

use anyhow::{Result, bail};
use lepiter_core::{KnowledgeBaseIndex, PageId, SearchHit, SearchMatchKind, render_page_to_text};

use super::format::truncate_chars;
use super::{ArgSpec, open_kb, parse_args};
use crate::util::matching_snippet;

const SPEC: ArgSpec<'static> = ArgSpec {
    usage: "usage: lepiter-cli search [--full-text] [--tsv] [--json] <query> [kb-path]\n\n\
            searches by title, id, and tags, optionally page content too.\n\n\
            flags:\n  \
              --full-text  also search page content\n  \
              --tsv        output as tab-separated values\n  \
              --json       include match kind alongside page metadata",
    toggles: &["--full-text", "--tsv", "--json"],
    valued: &[],
};

pub fn run_search(args: Vec<String>) -> Result<()> {
    let Some(args) = parse_args(args, &SPEC)? else {
        return Ok(());
    };
    let tsv = args.has("--tsv");
    let json = args.has("--json");

    let Some(query) = args.positional(0) else {
        bail!("missing required argument: <query>");
    };
    let query = query.trim().to_string();
    if query.is_empty() {
        bail!("query must not be empty");
    }

    let index = open_kb(&args.kb_path(1))?;
    let hits = index.search_hits(&query, args.has("--full-text"));
    let snippets = content_snippets(&index, &hits, &query.to_lowercase());

    let mut out = std::io::stdout().lock();
    if json {
        write_search_json(&mut out, &index, &hits, &snippets);
    } else if tsv {
        write_search_tsv(&mut out, &index, &hits, &snippets);
    } else {
        write_search_plain(&mut out, &index, &hits, &snippets);
    }

    Ok(())
}

/// Renders each content-match hit to text and extracts a matching-context
/// snippet, keyed by page id.
///
/// Title and tag hits carry no snippet and are skipped. A content hit whose
/// rendered text does not contain the needle — the content match is decided on
/// un-rendered node text, so rendering can move the match out of reach — is
/// simply absent from the map, and callers render an empty snippet for it.
fn content_snippets(
    index: &KnowledgeBaseIndex,
    hits: &[SearchHit],
    needle_lower: &str,
) -> HashMap<PageId, String> {
    let mut snippets = HashMap::new();
    for hit in hits {
        if hit.kind != SearchMatchKind::Content {
            continue;
        }
        let Ok(page) = index.load_page(&hit.id) else {
            continue;
        };
        let raw = render_page_to_text(&page);
        if let Some(snippet) = matching_snippet(&raw, needle_lower) {
            snippets.insert(hit.id.clone(), snippet);
        }
    }
    snippets
}

fn write_search_json(
    out: &mut impl Write,
    index: &KnowledgeBaseIndex,
    hits: &[SearchHit],
    snippets: &HashMap<PageId, String>,
) {
    let enriched: Vec<serde_json::Value> = hits
        .iter()
        .filter_map(|hit| {
            index.pages.get(&hit.id).map(|meta| {
                serde_json::json!({
                    "id": hit.id,
                    "title": meta.title,
                    "kind": hit.kind,
                    "tags": meta.tags,
                    "snippet": snippets.get(&hit.id).map(String::as_str).unwrap_or(""),
                })
            })
        })
        .collect();
    let _ = writeln!(out, "{}", serde_json::to_string_pretty(&enriched).unwrap());
}

fn write_search_tsv(
    out: &mut impl Write,
    index: &KnowledgeBaseIndex,
    hits: &[SearchHit],
    snippets: &HashMap<PageId, String>,
) {
    let hit_by_id = hits
        .iter()
        .map(|hit| {
            let kind = match hit.kind {
                SearchMatchKind::Title => "title",
                SearchMatchKind::Tag => "tag",
                SearchMatchKind::Content => "content",
            };
            (&hit.id, kind)
        })
        .collect::<HashMap<_, _>>();

    for meta in index.sorted_pages() {
        if let Some(kind) = hit_by_id.get(&meta.id) {
            // Tabs would split the snippet across columns; flatten them so the
            // row keeps exactly four fields.
            let snippet = snippets
                .get(&meta.id)
                .map(|s| s.replace('\t', " "))
                .unwrap_or_default();
            let _ = writeln!(out, "{}\t{}\t{}\t{}", meta.title, meta.id, kind, snippet);
        }
    }
}

fn write_search_plain(
    out: &mut impl Write,
    index: &KnowledgeBaseIndex,
    hits: &[SearchHit],
    snippets: &HashMap<PageId, String>,
) {
    let hit_by_id = hits
        .iter()
        .map(|hit| {
            let kind = match hit.kind {
                SearchMatchKind::Title => "title",
                SearchMatchKind::Tag => "tag",
                SearchMatchKind::Content => "content",
            };
            (&hit.id, kind)
        })
        .collect::<HashMap<_, _>>();

    let title_width = index
        .sorted_pages()
        .iter()
        .map(|m| m.title.chars().count())
        .max()
        .unwrap_or(5)
        .clamp(5, 64);

    let _ = writeln!(
        out,
        "{:<width$}  {:<36}  match",
        "title",
        "id",
        width = title_width
    );
    let _ = writeln!(
        out,
        "{:-<width$}  {:-<36}  {:-<7}",
        "",
        "",
        "",
        width = title_width
    );
    for meta in index.sorted_pages() {
        if let Some(kind) = hit_by_id.get(&meta.id) {
            let _ = writeln!(
                out,
                "{:<width$}  {:<36}  {}",
                truncate_chars(&meta.title, title_width),
                meta.id,
                kind,
                width = title_width
            );
            // The ~120-char snippet would blow out the fixed-width columns, so
            // it goes on its own indented line beneath the content-match row.
            if let Some(snippet) = snippets.get(&meta.id) {
                let _ = writeln!(out, "    {snippet}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lepiter_core::KnowledgeBase;

    fn make_test_kb(
        pages: &[(&str, &str, &[&str], &str)],
    ) -> (std::path::PathBuf, KnowledgeBaseIndex) {
        // `tempfile` picks the name: a timestamp cannot, because `SystemTime::now`
        // ticks at microsecond granularity here, so two tests in the same process
        // stamp the same value and share a directory.
        let dir = tempfile::Builder::new()
            .prefix("lepiter-search-test-")
            .tempdir()
            .expect("temp dir")
            .keep();
        for (id, title, tags, body) in pages {
            let tags_json: Vec<serde_json::Value> =
                tags.iter().map(|t| serde_json::json!(t)).collect();
            let content = serde_json::json!({
                "uid": {"uuid": id},
                "pageType": {"title": title},
                "tags": tags_json,
                "children": {"items": [
                    {"__type": "textSnippet", "string": body}
                ]}
            });
            let file_path = dir.join(format!("{id}.lepiter"));
            std::fs::write(&file_path, serde_json::to_vec(&content).unwrap()).unwrap();
        }
        let index = KnowledgeBase::open(&dir).unwrap();
        (dir, index)
    }

    fn output_string(f: impl FnOnce(&mut Vec<u8>)) -> String {
        let mut buf = Vec::new();
        f(&mut buf);
        String::from_utf8(buf).unwrap()
    }

    /// Build the snippet map the writers expect, mirroring `run_search`.
    fn snips(
        index: &KnowledgeBaseIndex,
        hits: &[SearchHit],
        query: &str,
    ) -> HashMap<PageId, String> {
        content_snippets(index, hits, &query.to_lowercase())
    }

    // --- JSON output ---

    #[test]
    fn search_json_title_match() {
        let (dir, index) = make_test_kb(&[
            ("p1", "Rust Guide", &[], "body"),
            ("p2", "Go Notes", &[], "body"),
        ]);
        let hits = index.search_hits("rust", false);
        let snippets = snips(&index, &hits, "rust");
        let out = output_string(|buf| write_search_json(buf, &index, &hits, &snippets));
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["id"], "p1");
        assert_eq!(parsed[0]["title"], "Rust Guide");
        assert_eq!(parsed[0]["kind"], "title");
        // A title hit carries no snippet.
        assert_eq!(parsed[0]["snippet"], "");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_json_tag_match_includes_tags() {
        let (dir, index) = make_test_kb(&[("p1", "Page One", &["rust", "cli"], "body")]);
        let hits = index.search_hits("cli", false);
        let snippets = snips(&index, &hits, "cli");
        let out = output_string(|buf| write_search_json(buf, &index, &hits, &snippets));
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["kind"], "tag");
        assert_eq!(parsed[0]["snippet"], "");
        let tags = parsed[0]["tags"].as_array().unwrap();
        assert!(tags.contains(&serde_json::json!("rust")));
        assert!(tags.contains(&serde_json::json!("cli")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_json_content_match() {
        let (dir, index) = make_test_kb(&[("p1", "Alpha", &[], "the quick brown fox")]);
        let hits = index.search_hits("fox", true);
        let snippets = snips(&index, &hits, "fox");
        let out = output_string(|buf| write_search_json(buf, &index, &hits, &snippets));
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["kind"], "content");
        // A content hit carries a matching-context snippet.
        assert_eq!(parsed[0]["snippet"], "the quick brown fox");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_json_content_hit_without_snippet_is_empty() {
        // Content matching happens on un-rendered node text, so a hit can
        // legitimately yield no rendered snippet. The writer must tolerate that
        // and emit an empty string, never omit the key.
        let (dir, index) = make_test_kb(&[("p1", "Alpha", &[], "the quick brown fox")]);
        let hits = index.search_hits("fox", true);
        let snippets = HashMap::new();
        let out = output_string(|buf| write_search_json(buf, &index, &hits, &snippets));
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed[0]["kind"], "content");
        assert_eq!(parsed[0]["snippet"], "");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_json_no_results() {
        let (dir, index) = make_test_kb(&[("p1", "Alpha", &[], "body")]);
        let hits = index.search_hits("zzzzz", false);
        let snippets = snips(&index, &hits, "zzzzz");
        let out = output_string(|buf| write_search_json(buf, &index, &hits, &snippets));
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
        assert!(parsed.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_json_multiple_match_kinds() {
        let (dir, index) = make_test_kb(&[
            ("p1", "Rust Guide", &[], "body"),
            ("p2", "Page Two", &["rust"], "body"),
            ("p3", "Page Three", &[], "mentions rust here"),
        ]);
        let hits = index.search_hits("rust", true);
        let snippets = snips(&index, &hits, "rust");
        let out = output_string(|buf| write_search_json(buf, &index, &hits, &snippets));
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed.len(), 3);
        // Ranked: title > tag > content.
        assert_eq!(parsed[0]["kind"], "title");
        assert_eq!(parsed[1]["kind"], "tag");
        assert_eq!(parsed[2]["kind"], "content");
        // Only the content hit gets a snippet.
        assert_eq!(parsed[0]["snippet"], "");
        assert_eq!(parsed[1]["snippet"], "");
        assert_eq!(parsed[2]["snippet"], "mentions rust here");
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- TSV output ---

    #[test]
    fn search_tsv_title_match() {
        let (dir, index) = make_test_kb(&[
            ("p1", "Rust Guide", &[], "body"),
            ("p2", "Go Notes", &[], "body"),
        ]);
        let hits = index.search_hits("rust", false);
        let snippets = snips(&index, &hits, "rust");
        let out = output_string(|buf| write_search_tsv(buf, &index, &hits, &snippets));
        // Use `lines()` rather than `trim()` here: an empty snippet leaves a
        // trailing tab, and `trim()` would strip it, hiding the 4th column.
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 1);
        let fields: Vec<&str> = lines[0].split('\t').collect();
        assert_eq!(fields.len(), 4, "expected title\\tid\\tkind\\tsnippet");
        assert_eq!(fields[0], "Rust Guide");
        assert_eq!(fields[1], "p1");
        assert_eq!(fields[2], "title");
        // Title hit — empty snippet column.
        assert_eq!(fields[3], "");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_tsv_content_match_has_snippet_column() {
        let (dir, index) = make_test_kb(&[("p1", "Alpha", &[], "the quick brown fox")]);
        let hits = index.search_hits("fox", true);
        let snippets = snips(&index, &hits, "fox");
        let out = output_string(|buf| write_search_tsv(buf, &index, &hits, &snippets));
        let fields: Vec<&str> = out.trim().split('\t').collect();
        assert_eq!(fields.len(), 4);
        assert_eq!(fields[2], "content");
        assert_eq!(fields[3], "the quick brown fox");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_tsv_snippet_strips_tab_chars() {
        // A tab inside the rendered snippet would spill into a fifth column;
        // the writer flattens it so the row stays exactly four fields.
        let (dir, index) = make_test_kb(&[("p1", "Alpha", &[], "fox\tbar baz")]);
        let hits = index.search_hits("fox", true);
        let snippets = snips(&index, &hits, "fox");
        let out = output_string(|buf| write_search_tsv(buf, &index, &hits, &snippets));
        let fields: Vec<&str> = out.trim().split('\t').collect();
        assert_eq!(fields.len(), 4, "tab in snippet must not add a column");
        assert_eq!(fields[3], "fox bar baz");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_tsv_snippet_strips_carriage_returns() {
        // A parser that treats a bare CR as a record separator would read one
        // row as three. Neither `lines()` nor `split('\t')` sees a mid-line CR,
        // so assert on the emitted bytes.
        let (dir, index) = make_test_kb(&[(
            "p1",
            "Alpha",
            &[],
            "first line\r\nfox is here\r\nthird line",
        )]);
        let hits = index.search_hits("fox", true);
        let snippets = snips(&index, &hits, "fox");
        let out = output_string(|buf| write_search_tsv(buf, &index, &hits, &snippets));
        assert!(!out.contains('\r'), "CR reached the TSV stream: {out:?}");
        let records: Vec<&str> = out.trim_end().split(['\r', '\n']).collect();
        assert_eq!(records.len(), 1, "row must stay one record: {out:?}");
        let fields: Vec<&str> = records[0].split('\t').collect();
        assert_eq!(fields.len(), 4, "CR in snippet must not add a record");
        assert!(fields[3].contains("fox is here"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_tsv_sorted_by_title() {
        let (dir, index) = make_test_kb(&[
            ("p1", "Zebra", &["common"], "body"),
            ("p2", "Alpha", &["common"], "body"),
        ]);
        let hits = index.search_hits("common", false);
        let snippets = snips(&index, &hits, "common");
        let out = output_string(|buf| write_search_tsv(buf, &index, &hits, &snippets));
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines.len(), 2);
        // sorted_pages iterates in title order.
        assert!(lines[0].starts_with("Alpha"));
        assert!(lines[1].starts_with("Zebra"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_tsv_no_results_empty() {
        let (dir, index) = make_test_kb(&[("p1", "Alpha", &[], "body")]);
        let hits = index.search_hits("zzzzz", false);
        let snippets = snips(&index, &hits, "zzzzz");
        let out = output_string(|buf| write_search_tsv(buf, &index, &hits, &snippets));
        assert!(out.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_tsv_content_match_label() {
        let (dir, index) = make_test_kb(&[("p1", "Alpha", &[], "the quick brown fox")]);
        let hits = index.search_hits("fox", true);
        let snippets = snips(&index, &hits, "fox");
        let out = output_string(|buf| write_search_tsv(buf, &index, &hits, &snippets));
        assert!(out.contains("\tcontent\t"));
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- Plain table output ---

    #[test]
    fn search_plain_has_header_and_separator() {
        let (dir, index) = make_test_kb(&[("p1", "Alpha", &[], "body")]);
        let hits = index.search_hits("alpha", false);
        let snippets = snips(&index, &hits, "alpha");
        let out = output_string(|buf| write_search_plain(buf, &index, &hits, &snippets));
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].contains("title"));
        assert!(lines[0].contains("id"));
        assert!(lines[0].contains("match"));
        // Second line is a separator of dashes.
        assert!(lines[1].contains("-----"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_plain_shows_match_kind() {
        let (dir, index) = make_test_kb(&[("p1", "Rust Guide", &[], "body")]);
        let hits = index.search_hits("rust", false);
        let snippets = snips(&index, &hits, "rust");
        let out = output_string(|buf| write_search_plain(buf, &index, &hits, &snippets));
        // Data row should contain the title, id, and match kind.
        assert!(out.contains("Rust Guide"));
        assert!(out.contains("p1"));
        assert!(out.contains("title"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_plain_content_shows_indented_snippet() {
        let (dir, index) = make_test_kb(&[("p1", "Alpha", &[], "the quick brown fox")]);
        let hits = index.search_hits("fox", true);
        let snippets = snips(&index, &hits, "fox");
        let out = output_string(|buf| write_search_plain(buf, &index, &hits, &snippets));
        // The snippet appears on its own indented line under the content row.
        assert!(
            out.contains("\n    the quick brown fox"),
            "expected indented snippet line, got: {out}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_plain_snippet_strips_carriage_returns() {
        // A CR returns the terminal cursor to column 0, overwriting the
        // indented snippet line and erasing the match it exists to show.
        let (dir, index) = make_test_kb(&[(
            "p1",
            "Alpha",
            &[],
            "first line\r\nfox is here\r\nthird line",
        )]);
        let hits = index.search_hits("fox", true);
        let snippets = snips(&index, &hits, "fox");
        let out = output_string(|buf| write_search_plain(buf, &index, &hits, &snippets));
        assert!(!out.contains('\r'), "CR reached the terminal: {out:?}");
        assert!(
            out.contains("\n    first line  fox is here  third line"),
            "expected a flattened snippet line, got: {out}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_plain_title_match_has_no_snippet_line() {
        let (dir, index) = make_test_kb(&[("p1", "Alpha", &[], "body")]);
        let hits = index.search_hits("alpha", false);
        let snippets = snips(&index, &hits, "alpha");
        let out = output_string(|buf| write_search_plain(buf, &index, &hits, &snippets));
        let lines: Vec<&str> = out.trim().lines().collect();
        // Header + separator + one data row, no indented snippet line.
        assert_eq!(lines.len(), 3);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_plain_no_results_only_header() {
        let (dir, index) = make_test_kb(&[("p1", "Alpha", &[], "body")]);
        let hits = index.search_hits("zzzzz", false);
        let snippets = snips(&index, &hits, "zzzzz");
        let out = output_string(|buf| write_search_plain(buf, &index, &hits, &snippets));
        let lines: Vec<&str> = out.trim().lines().collect();
        // Header + separator only, no data rows.
        assert_eq!(lines.len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_plain_title_width_clamped_to_min_5() {
        // Single-char title — width should be clamped to at least 5.
        let (dir, index) = make_test_kb(&[("p1", "A", &[], "body")]);
        let hits = index.search_hits("a", false);
        let snippets = snips(&index, &hits, "a");
        let out = output_string(|buf| write_search_plain(buf, &index, &hits, &snippets));
        let header = out.lines().next().unwrap();
        // "title" is 5 chars and should be left-padded to at least 5.
        assert!(header.starts_with("title"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
