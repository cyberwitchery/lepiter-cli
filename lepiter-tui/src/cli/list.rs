use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use lepiter_core::{KnowledgeBase, KnowledgeBaseIndex};

use super::format::truncate_chars;

pub fn run_list(args: Vec<String>) -> Result<()> {
    let mut tsv = false;
    let mut json = false;
    let mut positional = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--tsv" => tsv = true,
            "--json" => json = true,
            _ if arg.starts_with('-') => {
                eprintln!("unknown flag: {arg}");
                std::process::exit(2);
            }
            _ => positional.push(arg),
        }
    }
    let kb_path = positional
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./lepiter"));
    let index = KnowledgeBase::open(&kb_path)
        .with_context(|| format!("failed to open knowledge base at {}", kb_path.display()))?;

    let mut out = std::io::stdout().lock();
    if json {
        write_list_json(&mut out, &index);
    } else if tsv {
        write_list_tsv(&mut out, &index);
    } else {
        write_list_plain(&mut out, &index);
    }
    Ok(())
}

fn write_list_json(out: &mut impl Write, index: &KnowledgeBaseIndex) {
    let pages: Vec<&lepiter_core::PageMeta> = index.sorted_pages();
    let _ = writeln!(out, "{}", serde_json::to_string_pretty(&pages).unwrap());
}

fn write_list_tsv(out: &mut impl Write, index: &KnowledgeBaseIndex) {
    for meta in index.sorted_pages() {
        let _ = writeln!(out, "{}\t{}", meta.title, meta.id);
    }
}

fn write_list_plain(out: &mut impl Write, index: &KnowledgeBaseIndex) {
    let title_width = index
        .sorted_pages()
        .iter()
        .map(|m| m.title.chars().count())
        .max()
        .unwrap_or(5)
        .clamp(5, 64);

    let _ = writeln!(out, "{:<width$}  id", "title", width = title_width);
    let _ = writeln!(out, "{:-<width$}  {:-<36}", "", "", width = title_width);
    for meta in index.sorted_pages() {
        let _ = writeln!(
            out,
            "{:<width$}  {}",
            truncate_chars(&meta.title, title_width),
            meta.id,
            width = title_width
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_kb(pages: &[(&str, &str, &str)]) -> (std::path::PathBuf, KnowledgeBaseIndex) {
        use std::time::{SystemTime, UNIX_EPOCH};
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("lepiter-list-test-{ts}"));
        std::fs::create_dir_all(&dir).unwrap();
        for (id, title, body) in pages {
            let content = serde_json::json!({
                "uid": {"uuid": id},
                "pageType": {"title": title},
                "tags": [],
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
    fn list_json_contains_all_pages() {
        let (dir, index) = make_test_kb(&[("p1", "Alpha", "body"), ("p2", "Beta", "body")]);
        let out = output_string(|buf| write_list_json(buf, &index));
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed.len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_json_sorted_by_title() {
        let (dir, index) = make_test_kb(&[
            ("p1", "Zebra", "body"),
            ("p2", "Alpha", "body"),
            ("p3", "Middle", "body"),
        ]);
        let out = output_string(|buf| write_list_json(buf, &index));
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
        let titles: Vec<&str> = parsed
            .iter()
            .map(|v| v["title"].as_str().unwrap())
            .collect();
        assert_eq!(titles, vec!["Alpha", "Middle", "Zebra"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_json_includes_id_and_title_fields() {
        let (dir, index) = make_test_kb(&[("p1", "Alpha", "body")]);
        let out = output_string(|buf| write_list_json(buf, &index));
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed[0]["id"], "p1");
        assert_eq!(parsed[0]["title"], "Alpha");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_json_excludes_internal_fields() {
        let (dir, index) = make_test_kb(&[("p1", "Alpha", "body")]);
        let out = output_string(|buf| write_list_json(buf, &index));
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
        // Lowercase internal fields must not appear.
        assert!(parsed[0].get("id_lower").is_none());
        assert!(parsed[0].get("title_lower").is_none());
        assert!(parsed[0].get("tags_lower").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_json_empty_kb() {
        let (dir, index) = make_test_kb(&[]);
        let out = output_string(|buf| write_list_json(buf, &index));
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
        assert!(parsed.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- TSV output ---

    #[test]
    fn list_tsv_contains_title_and_id() {
        let (dir, index) = make_test_kb(&[("p1", "Alpha", "body")]);
        let out = output_string(|buf| write_list_tsv(buf, &index));
        let fields: Vec<&str> = out.trim().split('\t').collect();
        assert_eq!(fields[0], "Alpha");
        assert_eq!(fields[1], "p1");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_tsv_sorted_by_title() {
        let (dir, index) = make_test_kb(&[("p1", "Zebra", "body"), ("p2", "Alpha", "body")]);
        let out = output_string(|buf| write_list_tsv(buf, &index));
        let lines: Vec<&str> = out.trim().lines().collect();
        assert!(lines[0].starts_with("Alpha"));
        assert!(lines[1].starts_with("Zebra"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_tsv_empty_kb() {
        let (dir, index) = make_test_kb(&[]);
        let out = output_string(|buf| write_list_tsv(buf, &index));
        assert!(out.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- Plain table output ---

    #[test]
    fn list_plain_has_header_and_separator() {
        let (dir, index) = make_test_kb(&[("p1", "Alpha", "body")]);
        let out = output_string(|buf| write_list_plain(buf, &index));
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].contains("title"));
        assert!(lines[0].contains("id"));
        assert!(lines[1].contains("-----"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_plain_contains_page_data() {
        let (dir, index) = make_test_kb(&[("p1", "Alpha", "body")]);
        let out = output_string(|buf| write_list_plain(buf, &index));
        let lines: Vec<&str> = out.lines().collect();
        // Header + separator + 1 data row.
        assert_eq!(lines.len(), 3);
        assert!(lines[2].contains("Alpha"));
        assert!(lines[2].contains("p1"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_plain_sorted_by_title() {
        let (dir, index) = make_test_kb(&[
            ("p1", "Zebra", "body"),
            ("p2", "Alpha", "body"),
            ("p3", "Middle", "body"),
        ]);
        let out = output_string(|buf| write_list_plain(buf, &index));
        let data_lines: Vec<&str> = out.lines().skip(2).collect();
        assert_eq!(data_lines.len(), 3);
        assert!(data_lines[0].contains("Alpha"));
        assert!(data_lines[1].contains("Middle"));
        assert!(data_lines[2].contains("Zebra"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_plain_empty_kb_only_header() {
        let (dir, index) = make_test_kb(&[]);
        let out = output_string(|buf| write_list_plain(buf, &index));
        let lines: Vec<&str> = out.trim().lines().collect();
        // Header + separator, no data rows.
        assert_eq!(lines.len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_plain_title_width_clamped_to_min_5() {
        let (dir, index) = make_test_kb(&[("p1", "A", "body")]);
        let out = output_string(|buf| write_list_plain(buf, &index));
        let header = out.lines().next().unwrap();
        // "title" is 5 chars, column should be at least 5 wide.
        assert!(header.starts_with("title"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
