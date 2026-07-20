use std::io::Write;

use anyhow::{Result, bail};
use lepiter_core::{KnowledgeBaseIndex, SearchHit, SearchMatchKind};

use super::format::truncate_chars;
use super::{ArgSpec, open_kb, parse_args};

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

    let mut out = std::io::stdout().lock();
    if json {
        write_search_json(&mut out, &index, &hits);
    } else if tsv {
        write_search_tsv(&mut out, &index, &hits);
    } else {
        write_search_plain(&mut out, &index, &hits);
    }

    Ok(())
}

fn write_search_json(out: &mut impl Write, index: &KnowledgeBaseIndex, hits: &[SearchHit]) {
    let enriched: Vec<serde_json::Value> = hits
        .iter()
        .filter_map(|hit| {
            index.pages.get(&hit.id).map(|meta| {
                serde_json::json!({
                    "id": hit.id,
                    "title": meta.title,
                    "kind": hit.kind,
                    "tags": meta.tags,
                })
            })
        })
        .collect();
    let _ = writeln!(out, "{}", serde_json::to_string_pretty(&enriched).unwrap());
}

fn write_search_tsv(out: &mut impl Write, index: &KnowledgeBaseIndex, hits: &[SearchHit]) {
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
        .collect::<std::collections::HashMap<_, _>>();

    for meta in index.sorted_pages() {
        if let Some(kind) = hit_by_id.get(&meta.id) {
            let _ = writeln!(out, "{}\t{}\t{}", meta.title, meta.id, kind);
        }
    }
}

fn write_search_plain(out: &mut impl Write, index: &KnowledgeBaseIndex, hits: &[SearchHit]) {
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
        .collect::<std::collections::HashMap<_, _>>();

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
        use std::time::{SystemTime, UNIX_EPOCH};
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("lepiter-search-test-{ts}"));
        std::fs::create_dir_all(&dir).unwrap();
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

    // --- JSON output ---

    #[test]
    fn search_json_title_match() {
        let (dir, index) = make_test_kb(&[
            ("p1", "Rust Guide", &[], "body"),
            ("p2", "Go Notes", &[], "body"),
        ]);
        let hits = index.search_hits("rust", false);
        let out = output_string(|buf| write_search_json(buf, &index, &hits));
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["id"], "p1");
        assert_eq!(parsed[0]["title"], "Rust Guide");
        assert_eq!(parsed[0]["kind"], "title");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_json_tag_match_includes_tags() {
        let (dir, index) = make_test_kb(&[("p1", "Page One", &["rust", "cli"], "body")]);
        let hits = index.search_hits("cli", false);
        let out = output_string(|buf| write_search_json(buf, &index, &hits));
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["kind"], "tag");
        let tags = parsed[0]["tags"].as_array().unwrap();
        assert!(tags.contains(&serde_json::json!("rust")));
        assert!(tags.contains(&serde_json::json!("cli")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_json_content_match() {
        let (dir, index) = make_test_kb(&[("p1", "Alpha", &[], "the quick brown fox")]);
        let hits = index.search_hits("fox", true);
        let out = output_string(|buf| write_search_json(buf, &index, &hits));
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["kind"], "content");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_json_no_results() {
        let (dir, index) = make_test_kb(&[("p1", "Alpha", &[], "body")]);
        let hits = index.search_hits("zzzzz", false);
        let out = output_string(|buf| write_search_json(buf, &index, &hits));
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
        let out = output_string(|buf| write_search_json(buf, &index, &hits));
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed.len(), 3);
        // Ranked: title > tag > content.
        assert_eq!(parsed[0]["kind"], "title");
        assert_eq!(parsed[1]["kind"], "tag");
        assert_eq!(parsed[2]["kind"], "content");
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
        let out = output_string(|buf| write_search_tsv(buf, &index, &hits));
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines.len(), 1);
        let fields: Vec<&str> = lines[0].split('\t').collect();
        assert_eq!(fields[0], "Rust Guide");
        assert_eq!(fields[1], "p1");
        assert_eq!(fields[2], "title");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_tsv_sorted_by_title() {
        let (dir, index) = make_test_kb(&[
            ("p1", "Zebra", &["common"], "body"),
            ("p2", "Alpha", &["common"], "body"),
        ]);
        let hits = index.search_hits("common", false);
        let out = output_string(|buf| write_search_tsv(buf, &index, &hits));
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
        let out = output_string(|buf| write_search_tsv(buf, &index, &hits));
        assert!(out.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_tsv_content_match_label() {
        let (dir, index) = make_test_kb(&[("p1", "Alpha", &[], "the quick brown fox")]);
        let hits = index.search_hits("fox", true);
        let out = output_string(|buf| write_search_tsv(buf, &index, &hits));
        assert!(out.contains("\tcontent\n"));
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- Plain table output ---

    #[test]
    fn search_plain_has_header_and_separator() {
        let (dir, index) = make_test_kb(&[("p1", "Alpha", &[], "body")]);
        let hits = index.search_hits("alpha", false);
        let out = output_string(|buf| write_search_plain(buf, &index, &hits));
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
        let out = output_string(|buf| write_search_plain(buf, &index, &hits));
        // Data row should contain the title, id, and match kind.
        assert!(out.contains("Rust Guide"));
        assert!(out.contains("p1"));
        assert!(out.contains("title"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_plain_no_results_only_header() {
        let (dir, index) = make_test_kb(&[("p1", "Alpha", &[], "body")]);
        let hits = index.search_hits("zzzzz", false);
        let out = output_string(|buf| write_search_plain(buf, &index, &hits));
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
        let out = output_string(|buf| write_search_plain(buf, &index, &hits));
        let header = out.lines().next().unwrap();
        // "title" is 5 chars and should be left-padded to at least 5.
        assert!(header.starts_with("title"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
