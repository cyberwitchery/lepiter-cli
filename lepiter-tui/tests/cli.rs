use std::path::PathBuf;
use std::process::{Command, Output};

/// Path to the test fixture corpus (7 pages with known content).
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("lepiter-core")
        .join("tests")
        .join("fixtures")
        .join("corpus")
}

/// Locate the compiled binary from the workspace target directory.
fn bin_path() -> PathBuf {
    // CARGO_BIN_EXE_lepiter-cli is set by cargo test for [[bin]] targets
    // in the same package.
    PathBuf::from(env!("CARGO_BIN_EXE_lepiter-cli"))
}

fn run(args: &[&str]) -> Output {
    Command::new(bin_path())
        .args(args)
        .output()
        .expect("failed to execute lepiter-cli")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

fn fixtures_path_str() -> String {
    fixtures_dir().display().to_string()
}

/// the corpus pages nothing links to, as (id, title), sorted by title.
const CORPUS_ORPHANS: [(&str, &str); 6] = [
    ("fixture-page-beta", "beta page"),
    ("fixture-page-gamma", "gamma unknown page"),
    ("fixture-page-media", "media page"),
    ("fixture-page-rewrite", "rewrite page"),
    ("fixture-page-tagonly", "tag only page"),
    ("fixture-page-word", "word page"),
];

fn corpus_orphans_json() -> serde_json::Value {
    CORPUS_ORPHANS
        .iter()
        .map(|(id, title)| serde_json::json!({ "id": id, "title": title }))
        .collect()
}

/// a directory no other test shares. `tempfile` picks the name: a timestamp
/// cannot, because `SystemTime::now` ticks at microsecond granularity here, so
/// two tests in the same process stamp the same value and collide. unlike the
/// old helper this creates the directory, which both callers went on to do.
fn unique_temp(tag: &str) -> PathBuf {
    tempfile::Builder::new()
        .prefix(&format!("lepiter-cli-{tag}-"))
        .tempdir()
        .expect("temp dir")
        .keep()
}

/// a temp kb holding one page, one text snippet per entry.
fn write_text_snippet_kb(tag: &str, id: &str, title: &str, snippets: &[&str]) -> PathBuf {
    let root = unique_temp(tag);
    std::fs::create_dir_all(&root).unwrap();
    let page = serde_json::json!({
        "uid": { "uuid": id },
        "pageType": { "title": title },
        "children": { "items": snippets
            .iter()
            .map(|s| serde_json::json!({ "__type": "textSnippet", "string": s }))
            .collect::<Vec<_>>() }
    });
    std::fs::write(
        root.join(format!("{id}.lepiter")),
        serde_json::to_vec(&page).unwrap(),
    )
    .unwrap();
    root
}

// ---------------------------------------------------------------------------
// info subcommand
// ---------------------------------------------------------------------------

mod info {
    use super::*;

    #[test]
    fn plain_output_contains_page_count() {
        let out = run(&["info", &fixtures_path_str()]);
        assert!(out.status.success());
        let text = stdout(&out);
        assert!(text.contains("pages: 7"), "expected 7 pages, got: {text}");
        assert!(text.contains("Knowledge Base"));
        assert!(text.contains("name: <unknown>"));
    }

    #[test]
    fn plain_output_shows_tag_count() {
        let out = run(&["info", &fixtures_path_str()]);
        assert!(out.status.success());
        let text = stdout(&out);
        assert!(
            text.contains("unique_tags: 3"),
            "expected 3 tags, got: {text}"
        );
    }

    #[test]
    fn json_output_parses_and_has_expected_fields() {
        let out = run(&["info", "--json", &fixtures_path_str()]);
        assert!(out.status.success());
        let json: serde_json::Value =
            serde_json::from_str(&stdout(&out)).expect("info --json should produce valid JSON");
        assert_eq!(json["pages"], 7);
        assert_eq!(json["name"], "<unknown>");
        assert_eq!(json["unique_tags"], 3);
        assert!(json["updated_range"].is_object());
    }

    #[test]
    fn json_detail_includes_extra_sections() {
        let out = run(&["info", "--json", "--detail", &fixtures_path_str()]);
        assert!(out.status.success());
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("valid JSON");
        assert_eq!(json["broken_links"], serde_json::json!([]));
        assert_eq!(json["orphan_pages"], corpus_orphans_json());
        assert_eq!(
            json["tag_distribution"],
            serde_json::json!({ "alpha": 1, "fixture": 2, "shelved": 1 })
        );
        // container types (page, snippets, children) are not snippet types.
        assert_eq!(
            json["snippet_types"],
            serde_json::json!({
                "elementSnippet": 1,
                "listSnippet": 1,
                "mysterySnippet": 1,
                "pharoLinkSnippet": 1,
                "pharoRewrite": 1,
                "pictureSnippet": 1,
                "pythonSnippet": 1,
                "quoteSnippet": 1,
                "textSnippet": 5,
                "wordSnippet": 1,
                "youtubeSnippet": 1
            })
        );
    }

    #[test]
    fn detail_text_shows_sections() {
        let out = run(&["info", "--detail", &fixtures_path_str()]);
        assert!(out.status.success());
        let text = stdout(&out);
        for header in [
            "Broken Links (0):",
            "Orphan Pages (6):",
            "Tag Distribution (3):",
            "Snippet Types (11):",
        ] {
            assert!(
                text.lines().any(|l| l == header),
                "expected `{header}`, got: {text}"
            );
        }
    }

    /// all subcommands take the first positional as the kb path
    #[test]
    fn first_positional_is_the_knowledge_base_path() {
        let out = run(&["info", &fixtures_path_str(), "/nonexistent/kb"]);
        assert!(
            out.status.success(),
            "expected the first path to win, got: {}",
            stderr(&out)
        );
        let text = stdout(&out);
        assert!(text.contains("pages: 7"), "expected 7 pages, got: {text}");
        assert!(
            text.contains(&format!("path: {}", fixtures_path_str())),
            "expected the first path to be reported, got: {text}"
        );
    }

    #[test]
    fn first_positional_wins_consistently_across_subcommands() {
        for cmd in ["info", "list", "ids", "links", "tags", "check"] {
            let out = run(&[cmd, &fixtures_path_str(), "/nonexistent/kb"]);
            let alone = run(&[cmd, &fixtures_path_str()]);
            assert_eq!(
                out.status.code(),
                alone.status.code(),
                "`{cmd}` should read the first path, got: {}",
                stderr(&out)
            );
            assert_eq!(stdout(&out), stdout(&alone), "`{cmd}` output differs");
        }
    }
}

// ---------------------------------------------------------------------------
// list subcommand
// ---------------------------------------------------------------------------

mod list {
    use super::*;

    #[test]
    fn plain_output_lists_every_page_sorted_by_title() {
        let out = run(&["list", &fixtures_path_str()]);
        assert!(out.status.success());
        let text = stdout(&out);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], format!("{:<18}  id", "title"));
        assert_eq!(lines[1], format!("{}  {}", "-".repeat(18), "-".repeat(36)));
        let titles: Vec<&str> = lines[2..]
            .iter()
            .filter_map(|l| l.split("  ").next())
            .collect();
        assert_eq!(
            titles,
            [
                "alpha page",
                "beta page",
                "gamma unknown page",
                "media page",
                "rewrite page",
                "tag only page",
                "word page",
            ]
        );
    }

    #[test]
    fn tsv_output_has_tab_separated_lines() {
        let out = run(&["list", "--tsv", &fixtures_path_str()]);
        assert!(out.status.success());
        let text = stdout(&out);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 7, "expected 7 TSV lines");
        for line in &lines {
            let parts: Vec<&str> = line.split('\t').collect();
            assert_eq!(parts.len(), 2, "expected 2 columns, got: {line}");
        }
        assert!(lines[0].starts_with("alpha page\t"));
    }

    #[test]
    fn json_output_is_valid_array_of_pages() {
        let out = run(&["list", "--json", &fixtures_path_str()]);
        assert!(out.status.success());
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("valid JSON");
        let arr = json.as_array().expect("should be an array");
        assert_eq!(arr.len(), 7);

        // Verify first page structure (sorted alphabetically → alpha page).
        let first = &arr[0];
        assert_eq!(first["id"], "fixture-page-alpha");
        assert_eq!(first["title"], "alpha page");
        assert!(first["path"].as_str().is_some());
        assert!(first.get("tags").is_some());
    }

    #[test]
    fn json_pages_have_expected_schema() {
        let out = run(&["list", "--json", &fixtures_path_str()]);
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        for page in json.as_array().unwrap() {
            assert!(page["id"].is_string(), "id should be a string");
            assert!(page["title"].is_string(), "title should be a string");
            assert!(page["path"].is_string(), "path should be a string");
            assert!(page["tags"].is_array(), "tags should be an array");
            // updated_at can be null or string
            assert!(
                page["updated_at"].is_null() || page["updated_at"].is_string(),
                "updated_at should be null or string"
            );
        }
    }

    #[test]
    fn json_alpha_page_has_tags() {
        let out = run(&["list", "--json", &fixtures_path_str()]);
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        let alpha = json
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == "fixture-page-alpha")
            .expect("alpha page should be in list");
        let tags: Vec<&str> = alpha["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap())
            .collect();
        assert!(tags.contains(&"fixture"));
        assert!(tags.contains(&"alpha"));
    }

    #[test]
    fn json_does_not_leak_internal_fields() {
        let out = run(&["list", "--json", &fixtures_path_str()]);
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        for page in json.as_array().unwrap() {
            assert!(
                page.get("id_lower").is_none(),
                "id_lower should not appear in JSON"
            );
            assert!(
                page.get("title_lower").is_none(),
                "title_lower should not appear in JSON"
            );
            assert!(
                page.get("tags_lower").is_none(),
                "tags_lower should not appear in JSON"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// search subcommand
// ---------------------------------------------------------------------------

mod search {
    use super::*;

    #[test]
    fn title_match() {
        let out = run(&["search", "--json", "alpha", &fixtures_path_str()]);
        assert!(out.status.success());
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["id"], "fixture-page-alpha");
        assert_eq!(arr[0]["kind"], "title");
    }

    #[test]
    fn tag_match() {
        // "shelved" is a tag on the tag-only page and appears in no title or id.
        let out = run(&["search", "--json", "shelved", &fixtures_path_str()]);
        assert!(out.status.success());
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["id"], "fixture-page-tagonly");
        assert_eq!(arr[0]["kind"], "tag");
        assert_eq!(
            arr[0]["snippet"], "",
            "tag hit should have an empty snippet"
        );
    }

    #[test]
    fn tag_match_survives_full_text() {
        let out = run(&[
            "search",
            "--full-text",
            "--json",
            "shelved",
            &fixtures_path_str(),
        ]);
        assert!(out.status.success());
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["kind"], "tag");
    }

    #[test]
    fn tag_match_tsv_reports_tag_kind() {
        let out = run(&["search", "--tsv", "shelved", &fixtures_path_str()]);
        assert!(out.status.success());
        let text = stdout(&out);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1);
        let parts: Vec<&str> = lines[0].split('\t').collect();
        assert_eq!(parts.len(), 4, "expected title\\tid\\tkind\\tsnippet");
        assert_eq!(parts[0], "tag only page");
        assert_eq!(parts[1], "fixture-page-tagonly");
        assert_eq!(parts[2], "tag");
        assert_eq!(parts[3], "");
    }

    #[test]
    fn tag_match_plain_reports_tag_kind() {
        let out = run(&["search", "shelved", &fixtures_path_str()]);
        assert!(out.status.success());
        let text = stdout(&out);
        let row = text
            .lines()
            .find(|l| l.contains("fixture-page-tagonly"))
            .unwrap_or_else(|| panic!("expected a result row, got: {text}"));
        assert_eq!(
            row.split_whitespace().last(),
            Some("tag"),
            "plain match column should read 'tag', got: {row}"
        );
    }

    #[test]
    fn full_text_content_match() {
        // "paragraph" only appears in rendered content of alpha page, not titles/tags.
        let out = run(&[
            "search",
            "--full-text",
            "--json",
            "paragraph",
            &fixtures_path_str(),
        ]);
        assert!(out.status.success());
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["id"], "fixture-page-alpha");
        assert_eq!(arr[0]["kind"], "content");
    }

    #[test]
    fn no_results_returns_empty_array() {
        let out = run(&[
            "search",
            "--json",
            "zzz_no_match_ever",
            &fixtures_path_str(),
        ]);
        assert!(out.status.success());
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        assert_eq!(json.as_array().unwrap().len(), 0);
    }

    #[test]
    fn tsv_output_has_four_columns() {
        let out = run(&["search", "--tsv", "alpha", &fixtures_path_str()]);
        assert!(out.status.success());
        let text = stdout(&out);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1);
        let parts: Vec<&str> = lines[0].split('\t').collect();
        assert_eq!(parts.len(), 4, "expected title\\tid\\tkind\\tsnippet");
        assert_eq!(parts[0], "alpha page");
        assert_eq!(parts[1], "fixture-page-alpha");
        assert_eq!(parts[2], "title");
        // Title match — the snippet column is present but empty.
        assert_eq!(parts[3], "");
    }

    #[test]
    fn full_text_snippet_holds_block_looking_prose_verbatim() {
        let root = write_text_snippet_kb(
            "search-verbatim",
            "verbatim-page",
            "Verbatim Page",
            &["- looks like a list but is prose", "para one\n\npara two"],
        );

        let out = run(&[
            "search",
            "--full-text",
            "--tsv",
            "prose",
            root.to_str().unwrap(),
        ]);
        assert!(out.status.success(), "search failed: {}", stderr(&out));
        let text = stdout(&out);
        let line = text.lines().next().expect("expected a result row");
        let snippet = line.split('\t').nth(3).expect("expected a snippet column");
        assert!(
            snippet.contains("- looks like a list but is prose") && !snippet.contains('\\'),
            "snippet should hold the prose verbatim, got: {snippet}"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn missing_query_fails() {
        let out = run(&["search"]);
        assert!(!out.status.success());
        let err = stderr(&out);
        assert!(err.contains("missing required argument"));
    }

    #[test]
    fn without_full_text_no_content_match() {
        // Without --full-text, searching for content-only text yields no results.
        let out = run(&["search", "--json", "paragraph", &fixtures_path_str()]);
        assert!(out.status.success());
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        assert_eq!(
            json.as_array().unwrap().len(),
            0,
            "metadata-only search should not match page content"
        );
    }
}

// ---------------------------------------------------------------------------
// show subcommand
// ---------------------------------------------------------------------------

mod show {
    use super::*;

    #[test]
    fn show_by_id_json() {
        let out = run(&[
            "show",
            "--json",
            "--id",
            "fixture-page-alpha",
            &fixtures_path_str(),
        ]);
        assert!(out.status.success());
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        assert_eq!(json["id"], "fixture-page-alpha");
        assert_eq!(json["title"], "alpha page");
        assert_eq!(json["tags"], serde_json::json!(["fixture", "alpha"]));
        assert!(json["updated_at"].is_string());

        // alpha page: heading, paragraph, list
        let content = json["content"].as_array().unwrap();
        assert_eq!(content.len(), 3);
        assert_eq!(content[0]["type"], "heading");
        assert_eq!(content[0]["level"], 1);
        assert_eq!(content[0]["text"], "alpha heading");

        assert_eq!(content[1]["type"], "paragraph");
        assert_eq!(content[1]["text"], "alpha paragraph");

        assert_eq!(content[2]["type"], "list");
        assert!(content[2]["items"].is_array());
    }

    #[test]
    fn show_by_title_json() {
        let out = run(&[
            "show",
            "--json",
            "--by-title",
            "alpha page",
            &fixtures_path_str(),
        ]);
        assert!(out.status.success());
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        assert_eq!(json["id"], "fixture-page-alpha");
    }

    #[test]
    fn show_title_default_resolution() {
        // Without --id or --by-title, defaults to title-based lookup.
        let out = run(&["show", "--json", "alpha page", &fixtures_path_str()]);
        assert!(out.status.success());
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        assert_eq!(json["id"], "fixture-page-alpha");
    }

    #[test]
    fn json_beta_page_has_code_link_quote() {
        let out = run(&[
            "show",
            "--json",
            "--id",
            "fixture-page-beta",
            &fixtures_path_str(),
        ]);
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        let content = json["content"].as_array().unwrap();
        let types: Vec<&str> = content
            .iter()
            .map(|n| n["type"].as_str().unwrap())
            .collect();
        assert_eq!(types, ["code", "link", "quote"]);
    }

    #[test]
    fn json_unknown_node_preserves_source_type() {
        let out = run(&[
            "show",
            "--json",
            "--id",
            "fixture-page-gamma",
            &fixtures_path_str(),
        ]);
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        let content = json["content"].as_array().unwrap();
        let unknown = content
            .iter()
            .find(|n| n["type"] == "unknown")
            .expect("gamma page should have an unknown node");
        assert_eq!(unknown["source_type"], "mysterySnippet");
        assert!(unknown["raw"].is_object());
    }

    #[test]
    fn plain_output_renders_markdown() {
        let out = run(&["show", "--id", "fixture-page-alpha", &fixtures_path_str()]);
        assert!(out.status.success());
        let text = stdout(&out);
        let body = text
            .split_once("---\n\n")
            .expect("show prints a header separator")
            .1;
        assert_eq!(
            body,
            "# alpha heading\n\nalpha paragraph\n\n- first item\n- second item\n"
        );
    }

    #[test]
    fn plain_output_prints_block_looking_prose_verbatim() {
        let root = write_text_snippet_kb(
            "show-verbatim",
            "verbatim-page",
            "Verbatim Page",
            &[
                "- looks like a list but is prose",
                "para one\n\npara two",
                "\\newcommand{\\foo}{bar}",
            ],
        );

        let out = run(&["show", "--id", "verbatim-page", root.to_str().unwrap()]);
        assert!(out.status.success(), "show failed: {}", stderr(&out));
        let text = stdout(&out);
        let body = text
            .split_once("---\n\n")
            .expect("show prints a header separator")
            .1;
        assert_eq!(
            body,
            "- looks like a list but is prose\n\npara one\n\npara two\n\n\\newcommand{\\foo}{bar}\n"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn open_links_shows_resolved_links() {
        let out = run(&[
            "show",
            "--open-links",
            "--id",
            "fixture-page-beta",
            &fixtures_path_str(),
        ]);
        assert!(out.status.success());
        let text = stdout(&out);
        let (_, listing) = text
            .split_once("resolved links:\n")
            .expect("expected a resolved links section");
        assert_eq!(
            listing.lines().collect::<Vec<_>>(),
            ["  [1] go alpha -> internal:fixture-page-alpha"]
        );
    }

    #[test]
    fn open_links_lists_an_images_target() {
        let root = write_text_snippet_kb(
            "show-image-links",
            "image-links-page",
            "Image Links",
            &["see ![diagram](attachments/x.png) and [docs](https://docs.rs)"],
        );
        std::fs::create_dir_all(root.join("attachments")).unwrap();
        std::fs::write(root.join("attachments").join("x.png"), b"png").unwrap();

        let out = run(&[
            "show",
            "--open-links",
            "--id",
            "image-links-page",
            &root.display().to_string(),
        ]);
        assert!(out.status.success());
        let text = stdout(&out);
        let listed = text
            .lines()
            .filter(|line| line.starts_with("  ["))
            .collect::<Vec<_>>();
        assert_eq!(listed.len(), 2, "{text}");
        assert!(
            listed[0].starts_with("  [1] diagram -> attachment:"),
            "{text}"
        );
        assert!(!listed[0].contains("(missing)"), "{text}");
        assert_eq!(listed[1], "  [2] docs -> external:https://docs.rs");

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn missing_value_fails() {
        let out = run(&["show"]);
        assert!(!out.status.success());
        let err = stderr(&out);
        assert!(err.contains("missing required argument"));
    }

    #[test]
    fn mutually_exclusive_flags_fail() {
        let out = run(&["show", "--id", "--by-title", "foo", &fixtures_path_str()]);
        assert!(!out.status.success());
        let err = stderr(&out);
        assert!(err.contains("mutually exclusive"));
    }

    #[test]
    fn not_found_title_fails() {
        let out = run(&["show", "nonexistent_title_xyz", &fixtures_path_str()]);
        assert!(!out.status.success());
        let err = stderr(&out);
        assert!(err.contains("no page found"));
    }

    #[test]
    fn ambiguous_title_fails() {
        // "page" is a partial match for all 7 fixture pages.
        let out = run(&["show", "page", &fixtures_path_str()]);
        assert!(!out.status.success());
        let err = stderr(&out);
        assert!(err.contains("ambiguous"));
    }
}

// ---------------------------------------------------------------------------
// links subcommand
// ---------------------------------------------------------------------------

mod links {
    use super::*;

    #[test]
    fn plain_output_shows_statistics_most_linked_and_isolated_pages() {
        let out = run(&["links", &fixtures_path_str()]);
        assert!(out.status.success());
        assert_eq!(
            stdout(&out),
            "Link Graph\n  pages: 7\n  links: 1\n\n\
             Most Linked Pages:\n     1  alpha page\n\n\
             Isolated Pages (5):\n  gamma unknown page\n  media page\n  rewrite page\n  \
             tag only page\n  word page\n"
        );
    }

    #[test]
    fn json_output_has_nodes_and_edges() {
        let out = run(&["links", "--json", &fixtures_path_str()]);
        assert!(out.status.success());
        let json: serde_json::Value =
            serde_json::from_str(&stdout(&out)).expect("links --json should produce valid JSON");
        assert!(json["nodes"].is_array());
        assert!(json["edges"].is_array());
        let edges = json["edges"].as_array().unwrap();
        assert_eq!(edges.len(), 1, "expected 1 edge");
        assert_eq!(edges[0]["source"], "fixture-page-beta");
        assert_eq!(edges[0]["target"], "fixture-page-alpha");
    }

    #[test]
    fn json_nodes_have_id_and_title() {
        let out = run(&["links", "--json", &fixtures_path_str()]);
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        assert_eq!(
            json["nodes"],
            serde_json::json!([
                { "id": "fixture-page-alpha", "title": "alpha page" },
                { "id": "fixture-page-beta", "title": "beta page" },
            ])
        );
    }

    #[test]
    fn dot_output_is_valid_digraph() {
        let out = run(&["links", "--dot", &fixtures_path_str()]);
        assert!(out.status.success());
        let text = stdout(&out);
        assert!(text.starts_with("digraph links {"));
        assert!(text.contains("rankdir=LR;"));
        assert!(text.contains("\"fixture-page-beta\" -> \"fixture-page-alpha\""));
        assert!(text.trim_end().ends_with('}'));
    }

    #[test]
    fn dot_output_includes_labels() {
        let out = run(&["links", "--dot", &fixtures_path_str()]);
        let text = stdout(&out);
        assert!(text.contains("[label=\"alpha page\"]"));
        assert!(text.contains("[label=\"beta page\"]"));
    }

    #[test]
    fn for_flag_filters_to_ego_graph() {
        let out = run(&["links", "--for", "alpha page", &fixtures_path_str()]);
        assert!(out.status.success());
        let text = stdout(&out);
        assert!(
            text.contains("ego: alpha page"),
            "should show ego label, got: {text}"
        );
        assert!(text.contains("links: 1"));
    }

    #[test]
    fn for_flag_with_json() {
        let out = run(&[
            "links",
            "--json",
            "--for",
            "alpha page",
            &fixtures_path_str(),
        ]);
        assert!(out.status.success());
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        let edges = json["edges"].as_array().unwrap();
        assert_eq!(edges.len(), 1);
        let nodes = json["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 2);
    }

    #[test]
    fn for_flag_unconnected_page_shows_zero_links() {
        let out = run(&["links", "--for", "word page", &fixtures_path_str()]);
        assert!(out.status.success());
        let text = stdout(&out);
        assert!(text.contains("links: 0"));
    }

    #[test]
    fn for_flag_nonexistent_page_fails() {
        let out = run(&["links", "--for", "nonexistent_xyz", &fixtures_path_str()]);
        assert_eq!(out.status.code(), Some(1));
        assert!(
            stderr(&out).contains("no page found with title matching `nonexistent_xyz`"),
            "got: {}",
            stderr(&out)
        );
    }

    #[test]
    fn dot_and_json_mutually_exclusive() {
        let out = run(&["links", "--dot", "--json", &fixtures_path_str()]);
        assert_eq!(out.status.code(), Some(1));
        assert!(
            stderr(&out).contains("--dot and --json are mutually exclusive"),
            "got: {}",
            stderr(&out)
        );
    }

    #[test]
    fn for_flag_missing_argument_fails() {
        // the kb path comes first, so `--for` is the last argument and has no value.
        let out = run(&["links", &fixtures_path_str(), "--for"]);
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(stderr(&out), "Error: --for requires a page argument\n");
    }
}

// ---------------------------------------------------------------------------
// tags subcommand
// ---------------------------------------------------------------------------

mod tags {
    use super::*;

    #[test]
    fn plain_output_shows_tag_counts_sorted_by_count_desc() {
        let out = run(&["tags", &fixtures_path_str()]);
        assert!(out.status.success());
        // "fixture" is on 2 pages; ties at 1 sort alphabetically.
        assert_eq!(
            stdout(&out),
            "Tags (3 unique)\n     2  fixture\n     1  alpha\n     1  shelved\n"
        );
    }

    #[test]
    fn json_output_is_valid_array() {
        let out = run(&["tags", "--json", &fixtures_path_str()]);
        assert!(out.status.success());
        let json: serde_json::Value =
            serde_json::from_str(&stdout(&out)).expect("tags --json should produce valid JSON");
        let arr = json.as_array().expect("should be an array");
        assert_eq!(arr.len(), 3);
        // Sorted by count descending, ties alphabetically: fixture (2), alpha (1), shelved (1).
        assert_eq!(arr[0]["tag"], "fixture");
        assert_eq!(arr[0]["count"], 2);
        assert_eq!(arr[1]["tag"], "alpha");
        assert_eq!(arr[1]["count"], 1);
        assert_eq!(arr[2]["tag"], "shelved");
        assert_eq!(arr[2]["count"], 1);
    }

    #[test]
    fn tsv_output_has_two_columns() {
        let out = run(&["tags", "--tsv", &fixtures_path_str()]);
        assert!(out.status.success());
        let text = stdout(&out);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines, ["fixture\t2", "alpha\t1", "shelved\t1"]);
    }

    #[test]
    fn for_flag_lists_pages_for_tag() {
        let out = run(&["tags", "--for", "fixture", &fixtures_path_str()]);
        assert!(out.status.success());
        let text = stdout(&out);
        assert!(
            text.contains("Pages tagged \"fixture\""),
            "should show header, got: {text}"
        );
        assert!(
            text.contains("(2)"),
            "fixture tag should match 2 pages, got: {text}"
        );
        assert!(text.contains("alpha page"));
        assert!(text.contains("beta page"));
    }

    #[test]
    fn for_flag_case_insensitive() {
        let out = run(&["tags", "--for", "FIXTURE", &fixtures_path_str()]);
        assert!(out.status.success());
        let text = stdout(&out);
        assert!(
            text.contains("(2)"),
            "case-insensitive match should find 2 pages"
        );
    }

    #[test]
    fn for_flag_json_outputs_page_metadata() {
        let out = run(&["tags", "--json", "--for", "fixture", &fixtures_path_str()]);
        assert!(out.status.success());
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("valid JSON");
        let arr = json.as_array().expect("should be an array");
        assert_eq!(arr.len(), 2);
        // Pages should be sorted alphabetically by title.
        assert_eq!(arr[0]["id"], "fixture-page-alpha");
        assert_eq!(arr[0]["title"], "alpha page");
        assert_eq!(arr[1]["id"], "fixture-page-beta");
        assert_eq!(arr[1]["title"], "beta page");
    }

    #[test]
    fn for_flag_tsv_outputs_title_and_id() {
        let out = run(&["tags", "--tsv", "--for", "fixture", &fixtures_path_str()]);
        assert!(out.status.success());
        let text = stdout(&out);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let parts: Vec<&str> = lines[0].split('\t').collect();
        assert_eq!(parts.len(), 2, "expected title\\tid");
        assert_eq!(parts[0], "alpha page");
        assert_eq!(parts[1], "fixture-page-alpha");
    }

    #[test]
    fn for_flag_no_match_shows_empty() {
        let out = run(&["tags", "--for", "nonexistent_tag_xyz", &fixtures_path_str()]);
        assert!(out.status.success());
        let text = stdout(&out);
        assert!(text.contains("(0)"), "non-matching tag should show 0 pages");
    }

    #[test]
    fn for_flag_json_no_match_returns_empty_array() {
        let out = run(&[
            "tags",
            "--json",
            "--for",
            "nonexistent_tag_xyz",
            &fixtures_path_str(),
        ]);
        assert!(out.status.success());
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        assert_eq!(json.as_array().unwrap().len(), 0);
    }

    #[test]
    fn for_flag_missing_argument_fails() {
        // the kb path comes first, so `--for` is the last argument and has no value.
        let out = run(&["tags", &fixtures_path_str(), "--for"]);
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(stderr(&out), "Error: --for requires a tag argument\n");
    }
}

// ---------------------------------------------------------------------------
// check subcommand
// ---------------------------------------------------------------------------

mod check {
    use super::*;

    #[test]
    fn plain_output_shows_summary() {
        let out = run(&["check", &fixtures_path_str()]);
        let text = stdout(&out);
        assert!(
            text.contains("Knowledge Base Check"),
            "should show header, got: {text}"
        );
        assert!(
            text.contains("broken_links: 0"),
            "expected 0 broken links, got: {text}"
        );
        assert!(
            text.contains("orphan_pages: 6"),
            "expected 6 orphan pages, got: {text}"
        );
        assert!(
            text.contains("load_errors: 0"),
            "expected 0 load errors, got: {text}"
        );
        assert!(
            text.contains("index_issues: 0"),
            "expected 0 index issues, got: {text}"
        );
    }

    #[test]
    fn plain_output_shows_no_broken_links() {
        let out = run(&["check", &fixtures_path_str()]);
        let text = stdout(&out);
        assert!(
            text.contains("Broken Links (0)"),
            "should show broken links header"
        );
        assert!(
            text.contains("(none)"),
            "no broken links expected, got: {text}"
        );
    }

    #[test]
    fn plain_output_lists_orphan_pages() {
        let out = run(&["check", &fixtures_path_str()]);
        let text = stdout(&out);
        assert!(text.contains("Orphan Pages (6)"));
        // alpha page is linked to by beta, so it should NOT be orphan
        assert!(
            !text.lines().any(|l| l.trim() == "alpha page"),
            "alpha page should not be orphan (linked by beta)"
        );
        // beta page links out but nobody links to it
        assert!(text.contains("beta page"), "beta should be orphan");
        assert!(
            text.contains("gamma unknown page"),
            "gamma should be orphan"
        );
        assert!(text.contains("word page"), "word should be orphan");
        assert!(
            text.contains("tag only page"),
            "tag only page should be orphan"
        );
    }

    #[test]
    fn exits_nonzero_when_issues_found() {
        let out = run(&["check", &fixtures_path_str()]);
        assert_eq!(
            out.status.code(),
            Some(1),
            "should exit 1 when orphan pages exist"
        );
    }

    #[test]
    fn json_output_lists_orphans_but_not_the_linked_page() {
        let out = run(&["check", "--json", &fixtures_path_str()]);
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        assert_eq!(json["broken_links"], serde_json::json!([]));
        // alpha is linked to by beta, so it is the one page missing here.
        assert_eq!(json["orphan_pages"], corpus_orphans_json());
    }

    // Tests using the broken-links fixture KB.

    fn broken_links_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("lepiter-core")
            .join("tests")
            .join("fixtures")
            .join("broken-links")
    }

    fn broken_links_path_str() -> String {
        broken_links_dir().display().to_string()
    }

    #[test]
    fn broken_links_fixture_plain_lists_broken_links_and_exits_1() {
        let out = run(&["check", &broken_links_path_str()]);
        assert_eq!(
            out.status.code(),
            Some(1),
            "should exit 1 when broken links exist"
        );
        let text = stdout(&out);
        let (_, section) = text
            .split_once("Broken Links (3):\n")
            .unwrap_or_else(|| panic!("expected 3 broken links, got: {text}"));
        assert_eq!(
            section
                .lines()
                .take_while(|l| !l.is_empty())
                .collect::<Vec<_>>(),
            [
                "  multi broken page -> page:bl-page-nope",
                "  multi broken page -> page:bl-page-also-nope",
                "  ok page -> page:bl-page-ghost",
            ]
        );
    }

    #[test]
    fn broken_links_fixture_json_reports_sources_orphans_and_no_errors() {
        let out = run(&["check", "--json", &broken_links_path_str()]);
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        assert_eq!(
            json["broken_links"],
            serde_json::json!([
                {
                    "source_id": "bl-page-multi",
                    "source_title": "multi broken page",
                    "target": "page:bl-page-nope"
                },
                {
                    "source_id": "bl-page-multi",
                    "source_title": "multi broken page",
                    "target": "page:bl-page-also-nope"
                },
                {
                    "source_id": "bl-page-ok",
                    "source_title": "ok page",
                    "target": "page:bl-page-ghost"
                },
            ])
        );
        // target is linked to by ok, so only ok and multi are orphans
        let orphan_ids: Vec<&str> = json["orphan_pages"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|p| p["id"].as_str())
            .collect();
        assert_eq!(orphan_ids, ["bl-page-multi", "bl-page-ok"]);
        assert_eq!(json["load_errors"], serde_json::json!([]));
        assert_eq!(json["index_issues"], serde_json::json!([]));
        assert_eq!(json["ok"], false);
    }
}

// ---------------------------------------------------------------------------
// check: lepiter.properties failure handling
// ---------------------------------------------------------------------------

mod check_properties {
    use super::*;
    use std::path::Path;

    fn temp_kb(tag: &str) -> PathBuf {
        tempfile::Builder::new()
            .prefix(&format!("lepiter-cli-props-{tag}-"))
            .tempdir()
            .expect("temp dir")
            .keep()
    }

    fn write_page(dir: &Path, id: &str, title: &str, body: &str) {
        let page = serde_json::json!({
            "uid": { "uuid": id },
            "pageType": { "title": title },
            "tags": [],
            "children": { "items": [ { "__type": "textSnippet", "string": body } ] }
        });
        std::fs::write(
            dir.join(format!("{id}.lepiter")),
            serde_json::to_vec(&page).unwrap(),
        )
        .unwrap();
    }

    /// Two mutually linking pages, so neither is a genuine orphan.
    fn write_linked_pair(dir: &Path) {
        write_page(dir, "a", "Page A", "see [B](page:b)");
        write_page(dir, "b", "Page B", "see [A](page:a)");
    }

    #[test]
    fn corrupt_properties_warns_and_reports_no_false_orphan() {
        let dir = temp_kb("corrupt");
        write_linked_pair(&dir);
        std::fs::write(dir.join("lepiter.properties"), b"{ not valid json").unwrap();

        let out = run(&["check", "--json", dir.to_str().unwrap()]);
        let err = stderr(&out);
        assert!(
            err.contains("lepiter.properties"),
            "corruption should be reported on stderr, got: {err}"
        );
        let json: serde_json::Value =
            serde_json::from_str(&stdout(&out)).expect("check --json should still emit valid JSON");
        assert_eq!(
            json["orphan_pages"].as_array().unwrap().len(),
            0,
            "linked pages must not be reported as orphans"
        );
        assert!(
            out.status.success(),
            "a corrupt config alone must not make check exit nonzero"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_properties_is_silent_with_no_false_orphan() {
        let dir = temp_kb("missing");
        write_linked_pair(&dir);

        let out = run(&["check", "--json", dir.to_str().unwrap()]);
        assert!(
            out.status.success(),
            "missing properties must not fail check, stderr: {}",
            stderr(&out)
        );
        assert!(
            stderr(&out).is_empty(),
            "a missing properties file must be silent, got: {}",
            stderr(&out)
        );
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        assert_eq!(json["orphan_pages"].as_array().unwrap().len(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unreadable_properties_surfaces_an_error() {
        let dir = temp_kb("unreadable");
        write_linked_pair(&dir);
        // A directory in place of the file makes the read fail with a non-NotFound error.
        std::fs::create_dir(dir.join("lepiter.properties")).unwrap();

        let out = run(&["check", dir.to_str().unwrap()]);
        assert!(
            !out.status.success(),
            "an unreadable properties file must surface an error"
        );
        let err = stderr(&out);
        assert!(
            err.contains("failed to read") && err.contains("lepiter.properties"),
            "error should name the unreadable file, got: {err}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn valid_toc_page_is_not_reported_as_orphan() {
        let dir = temp_kb("valid-toc");
        write_page(
            &dir,
            "toc",
            "Table of Contents",
            "see [Content](page:content)",
        );
        write_page(&dir, "content", "Content", "body");
        std::fs::write(
            dir.join("lepiter.properties"),
            br#"{"tableOfContents":"toc"}"#,
        )
        .unwrap();

        let out = run(&["check", "--json", dir.to_str().unwrap()]);
        assert!(
            out.status.success(),
            "a KB whose only unlinked page is the TOC should pass, stderr: {}",
            stderr(&out)
        );
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        let orphan_ids: Vec<&str> = json["orphan_pages"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|p| p["id"].as_str())
            .collect();
        assert!(
            !orphan_ids.contains(&"toc"),
            "the TOC page must be excluded from orphans, got: {orphan_ids:?}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

// ---------------------------------------------------------------------------
// help / usage
// ---------------------------------------------------------------------------

mod help {
    use super::*;

    #[test]
    fn no_args_prints_usage() {
        let out = run(&[]);
        assert!(out.status.success());
        let err = stderr(&out);
        assert!(err.contains("lepiter-cli"));
        assert!(err.contains("subcommands:"));
    }

    #[test]
    fn help_flag_prints_usage() {
        let out = run(&["--help"]);
        assert!(out.status.success());
        let err = stderr(&out);
        assert!(err.contains("subcommands:"));
    }

    #[test]
    fn unknown_subcommand_fails() {
        let out = run(&["bogus_command"]);
        assert_eq!(out.status.code(), Some(2));
        let err = stderr(&out);
        assert!(err.contains("unknown subcommand"));
    }

    #[test]
    fn every_subcommand_accepts_help() {
        for cmd in [
            "info", "list", "ids", "search", "show", "links", "tags", "check", "export", "import",
        ] {
            for flag in ["--help", "-h"] {
                let out = run(&[cmd, flag]);
                assert!(
                    out.status.success(),
                    "`{cmd} {flag}` should exit 0, got {:?}",
                    out.status
                );
                let err = stderr(&out);
                assert!(
                    err.contains(&format!("usage: lepiter-cli {cmd}")),
                    "`{cmd} {flag}` should print its usage, got: {err}"
                );
            }
        }
    }

    #[test]
    fn help_takes_precedence_over_a_knowledge_base_path() {
        let out = run(&["list", &fixtures_path_str(), "--help"]);
        assert!(out.status.success());
        assert!(stderr(&out).contains("usage: lepiter-cli list"));
        assert!(stdout(&out).is_empty(), "help should not list pages");
    }

    #[test]
    fn unknown_flag_exits_with_status_2() {
        for cmd in [
            "info", "list", "ids", "search", "show", "links", "tags", "check", "export", "import",
        ] {
            let out = run(&[cmd, "--badarg", &fixtures_path_str()]);
            assert_eq!(
                out.status.code(),
                Some(2),
                "`{cmd} --badarg` should be a usage error"
            );
            let err = stderr(&out);
            assert!(
                err.contains("unknown flag: --badarg"),
                "`{cmd} --badarg` should name the flag, got: {err}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// export -> import round-trip
// ---------------------------------------------------------------------------

mod import_export {
    use super::*;

    // the round-trip must preserve the snippet `__type`
    #[test]
    fn export_then_import_preserves_code_snippet_type() {
        let root = unique_temp("roundtrip");
        let src_kb = root.join("src");
        let exported = root.join("exported");
        let dst_kb = root.join("dst");
        std::fs::create_dir_all(&src_kb).unwrap();

        let page = serde_json::json!({
            "uid": { "uuid": "robo-page" },
            "pageType": { "title": "Robo Page" },
            "children": { "items": [
                { "__type": "robocoderMetamodelSnippet", "code": "Metamodel new build" }
            ] }
        });
        std::fs::write(
            src_kb.join("robo-page.lepiter"),
            serde_json::to_vec(&page).unwrap(),
        )
        .unwrap();

        let export_out = run(&[
            "export",
            exported.to_str().unwrap(),
            src_kb.to_str().unwrap(),
        ]);
        assert!(
            export_out.status.success(),
            "export failed: {}",
            stderr(&export_out)
        );

        let import_out = run(&[
            "import",
            exported.to_str().unwrap(),
            dst_kb.to_str().unwrap(),
        ]);
        assert!(
            import_out.status.success(),
            "import failed: {}",
            stderr(&import_out)
        );

        let pages: Vec<_> = std::fs::read_dir(&dst_kb)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "lepiter"))
            .collect();
        assert_eq!(pages.len(), 1, "expected exactly one imported page");

        let imported: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dst_kb.join("robo-page.lepiter")).unwrap(),
        )
        .unwrap();
        let items = imported["children"]["items"].as_array().unwrap();
        let snippet = items
            .iter()
            .find(|i| i["__type"] == "robocoderMetamodelSnippet")
            .expect("robocoderMetamodelSnippet must survive the round-trip");
        assert_eq!(snippet["code"], "Metamodel new build");

        std::fs::remove_dir_all(&root).unwrap();
    }
}
