use std::path::PathBuf;
use std::process::{Command, Output};

/// Path to the test fixture corpus (6 pages with known content).
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
        assert!(text.contains("pages: 6"), "expected 6 pages, got: {text}");
        assert!(text.contains("Knowledge Base"));
        assert!(text.contains("name: <unknown>"));
    }

    #[test]
    fn plain_output_shows_tag_count() {
        let out = run(&["info", &fixtures_path_str()]);
        assert!(out.status.success());
        let text = stdout(&out);
        assert!(
            text.contains("unique_tags: 2"),
            "expected 2 tags, got: {text}"
        );
    }

    #[test]
    fn json_output_parses_and_has_expected_fields() {
        let out = run(&["info", "--json", &fixtures_path_str()]);
        assert!(out.status.success());
        let json: serde_json::Value =
            serde_json::from_str(&stdout(&out)).expect("info --json should produce valid JSON");
        assert_eq!(json["pages"], 6);
        assert_eq!(json["name"], "<unknown>");
        assert_eq!(json["unique_tags"], 2);
        assert!(json["updated_range"].is_object());
    }

    #[test]
    fn json_detail_includes_extra_sections() {
        let out = run(&["info", "--json", "--detail", &fixtures_path_str()]);
        assert!(out.status.success());
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("valid JSON");
        assert!(json["broken_links"].is_array());
        assert!(json["orphan_pages"].is_array());
        assert!(json["tag_distribution"].is_object());
        assert!(json["snippet_types"].is_object());
    }

    #[test]
    fn detail_text_shows_sections() {
        let out = run(&["info", "--detail", &fixtures_path_str()]);
        assert!(out.status.success());
        let text = stdout(&out);
        assert!(text.contains("Broken Links"));
        assert!(text.contains("Orphan Pages"));
        assert!(text.contains("Tag Distribution"));
        assert!(text.contains("Snippet Types"));
    }

    #[test]
    fn unknown_flag_exits_nonzero() {
        let out = run(&["info", "--badarg", &fixtures_path_str()]);
        assert!(!out.status.success());
        let err = stderr(&out);
        assert!(
            err.contains("unknown flag"),
            "expected unknown flag error, got: {err}"
        );
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
        assert!(text.contains("pages: 6"), "expected 6 pages, got: {text}");
        assert!(
            text.contains(&format!("path: {}", fixtures_path_str())),
            "expected the first path to be reported, got: {text}"
        );
    }

    #[test]
    fn first_positional_wins_consistently_across_subcommands() {
        for cmd in ["info", "list", "ids", "links", "tags", "check"] {
            let out = run(&[cmd, &fixtures_path_str(), "/nonexistent/kb"]);
            let err = stderr(&out);
            assert!(
                !err.contains("failed to open knowledge base"),
                "`{cmd}` should read the first path, got: {err}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// list subcommand
// ---------------------------------------------------------------------------

mod list {
    use super::*;

    #[test]
    fn plain_output_has_header_and_all_pages() {
        let out = run(&["list", &fixtures_path_str()]);
        assert!(out.status.success());
        let text = stdout(&out);
        assert!(text.contains("title"));
        assert!(text.contains("id"));
        // All 6 fixture pages should appear.
        assert!(text.contains("alpha page"));
        assert!(text.contains("beta page"));
        assert!(text.contains("gamma unknown page"));
        assert!(text.contains("media page"));
        assert!(text.contains("rewrite page"));
        assert!(text.contains("word page"));
    }

    #[test]
    fn plain_output_is_sorted_alphabetically() {
        let out = run(&["list", &fixtures_path_str()]);
        let text = stdout(&out);
        let titles: Vec<&str> = text
            .lines()
            .skip(2) // skip header and separator
            .filter(|l| !l.is_empty())
            .filter_map(|l| l.split("  ").next())
            .map(|t| t.trim())
            .collect();
        let mut sorted = titles.clone();
        sorted.sort();
        assert_eq!(titles, sorted, "pages should be sorted alphabetically");
    }

    #[test]
    fn tsv_output_has_tab_separated_lines() {
        let out = run(&["list", "--tsv", &fixtures_path_str()]);
        assert!(out.status.success());
        let text = stdout(&out);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 6, "expected 6 TSV lines");
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
        assert_eq!(arr.len(), 6);

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

    #[test]
    fn unknown_flag_exits_nonzero() {
        let out = run(&["list", "--badarg", &fixtures_path_str()]);
        assert!(!out.status.success());
        let err = stderr(&out);
        assert!(
            err.contains("unknown flag"),
            "expected unknown flag error, got: {err}"
        );
    }
}

// ---------------------------------------------------------------------------
// ids subcommand
// ---------------------------------------------------------------------------

mod ids {
    use super::*;

    #[test]
    fn unknown_flag_exits_nonzero() {
        let out = run(&["ids", "--badarg", &fixtures_path_str()]);
        assert!(!out.status.success());
        let err = stderr(&out);
        assert!(
            err.contains("unknown flag"),
            "expected unknown flag error, got: {err}"
        );
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
        let out = run(&["search", "--json", "alpha", &fixtures_path_str()]);
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        let alpha = json
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["id"] == "fixture-page-alpha")
            .unwrap();
        // "alpha" matches on both title and tag — title wins.
        assert_eq!(alpha["kind"], "title");
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
    fn full_text_json_includes_matching_snippet() {
        // "paragraph" only appears in rendered content, so the content hit
        // should carry a snippet quoting the surrounding text.
        let out = run(&[
            "search",
            "--full-text",
            "--json",
            "paragraph",
            &fixtures_path_str(),
        ]);
        assert!(out.status.success());
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        let hit = &json.as_array().unwrap()[0];
        assert_eq!(hit["kind"], "content");
        let snippet = hit["snippet"].as_str().expect("snippet should be a string");
        assert!(
            snippet.contains("paragraph"),
            "snippet should quote the match, got: {snippet}"
        );
    }

    #[test]
    fn title_match_json_has_empty_snippet() {
        let out = run(&["search", "--json", "alpha", &fixtures_path_str()]);
        assert!(out.status.success());
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        let hit = &json.as_array().unwrap()[0];
        assert_eq!(hit["kind"], "title");
        assert_eq!(hit["snippet"], "", "title hit should have an empty snippet");
    }

    #[test]
    fn full_text_tsv_snippet_is_fourth_column() {
        let out = run(&[
            "search",
            "--full-text",
            "--tsv",
            "paragraph",
            &fixtures_path_str(),
        ]);
        assert!(out.status.success());
        let text = stdout(&out);
        let line = text.lines().next().expect("expected a result row");
        let parts: Vec<&str> = line.split('\t').collect();
        assert_eq!(parts.len(), 4, "expected four columns, got: {line}");
        assert_eq!(parts[2], "content");
        assert!(
            parts[3].contains("paragraph"),
            "fourth column should hold the snippet, got: {}",
            parts[3]
        );
    }

    #[test]
    fn full_text_plain_shows_indented_snippet() {
        let out = run(&["search", "--full-text", "paragraph", &fixtures_path_str()]);
        assert!(out.status.success());
        let text = stdout(&out);
        // The snippet appears on its own line, indented, beneath the match row.
        assert!(
            text.lines()
                .any(|l| l.starts_with("    ") && l.contains("paragraph")),
            "expected an indented snippet line, got: {text}"
        );
    }

    #[test]
    fn plain_output_shows_match_column() {
        let out = run(&["search", "alpha", &fixtures_path_str()]);
        assert!(out.status.success());
        let text = stdout(&out);
        assert!(text.contains("match"), "header should contain 'match'");
        assert!(text.contains("title"), "should show match kind");
        assert!(text.contains("alpha page"));
    }

    #[test]
    fn json_results_include_tags() {
        let out = run(&["search", "--json", "alpha", &fixtures_path_str()]);
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        let hit = &json.as_array().unwrap()[0];
        assert!(hit["tags"].is_array());
        let tags: Vec<&str> = hit["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap())
            .collect();
        assert!(tags.contains(&"fixture"));
        assert!(tags.contains(&"alpha"));
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

    #[test]
    fn unknown_flag_exits_nonzero() {
        let out = run(&["search", "--badarg", &fixtures_path_str()]);
        assert!(!out.status.success());
        let err = stderr(&out);
        assert!(
            err.contains("unknown flag"),
            "expected unknown flag error, got: {err}"
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
        assert!(json["content"].is_array());
        assert!(!json["content"].as_array().unwrap().is_empty());
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
    fn json_page_content_has_type_tags() {
        let out = run(&[
            "show",
            "--json",
            "--id",
            "fixture-page-alpha",
            &fixtures_path_str(),
        ]);
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        let content = json["content"].as_array().unwrap();

        // alpha page: heading, paragraph, list
        assert_eq!(content[0]["type"], "heading");
        assert_eq!(content[0]["level"], 1);
        assert_eq!(content[0]["text"], "alpha heading");

        assert_eq!(content[1]["type"], "paragraph");
        assert_eq!(content[1]["text"], "alpha paragraph");

        assert_eq!(content[2]["type"], "list");
        assert!(content[2]["items"].is_array());
    }

    #[test]
    fn json_page_includes_tags_and_timestamp() {
        let out = run(&[
            "show",
            "--json",
            "--id",
            "fixture-page-alpha",
            &fixtures_path_str(),
        ]);
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        let tags: Vec<&str> = json["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap())
            .collect();
        assert!(tags.contains(&"fixture"));
        assert!(tags.contains(&"alpha"));
        assert!(json["updated_at"].is_string());
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
        assert!(types.contains(&"code"), "beta should have code node");
        assert!(types.contains(&"link"), "beta should have link node");
        assert!(types.contains(&"quote"), "beta should have quote node");
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
        assert!(text.contains("# alpha heading"));
        assert!(text.contains("alpha paragraph"));
        assert!(text.contains("first item"));
        assert!(text.contains("second item"));
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
        assert!(text.contains("resolved links:"));
        assert!(text.contains("internal:fixture-page-alpha"));
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
        // "page" is a partial match for all 6 fixture pages.
        let out = run(&["show", "page", &fixtures_path_str()]);
        assert!(!out.status.success());
        let err = stderr(&out);
        assert!(err.contains("ambiguous"));
    }

    #[test]
    fn unknown_flag_exits_nonzero() {
        let out = run(&["show", "--badarg", &fixtures_path_str()]);
        assert!(!out.status.success());
        let err = stderr(&out);
        assert!(
            err.contains("unknown flag"),
            "expected unknown flag error, got: {err}"
        );
    }
}

// ---------------------------------------------------------------------------
// links subcommand
// ---------------------------------------------------------------------------

mod links {
    use super::*;

    #[test]
    fn plain_output_shows_statistics() {
        let out = run(&["links", &fixtures_path_str()]);
        assert!(out.status.success());
        let text = stdout(&out);
        assert!(text.contains("Link Graph"), "should show header");
        assert!(text.contains("pages: 6"), "expected 6 pages, got: {text}");
        assert!(text.contains("links: 1"), "expected 1 link, got: {text}");
    }

    #[test]
    fn plain_output_shows_most_linked() {
        let out = run(&["links", &fixtures_path_str()]);
        let text = stdout(&out);
        assert!(
            text.contains("Most Linked Pages"),
            "should show most linked section"
        );
        assert!(
            text.contains("alpha page"),
            "alpha page should be most linked"
        );
    }

    #[test]
    fn plain_output_shows_isolated_pages() {
        let out = run(&["links", &fixtures_path_str()]);
        let text = stdout(&out);
        assert!(
            text.contains("Isolated Pages"),
            "should show isolated section"
        );
        assert!(
            text.contains("gamma unknown page"),
            "gamma should be isolated"
        );
        assert!(text.contains("word page"), "word should be isolated");
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
        for node in json["nodes"].as_array().unwrap() {
            assert!(node["id"].is_string(), "node should have id");
            assert!(node["title"].is_string(), "node should have title");
        }
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
        assert!(!out.status.success());
    }

    #[test]
    fn dot_and_json_mutually_exclusive() {
        let out = run(&["links", "--dot", "--json", &fixtures_path_str()]);
        assert!(!out.status.success());
    }

    #[test]
    fn for_flag_missing_argument_fails() {
        let out = run(&["links", "--for"]);
        assert!(!out.status.success());
    }

    #[test]
    fn unknown_flag_exits_nonzero() {
        let out = run(&["links", "--badarg", &fixtures_path_str()]);
        assert!(!out.status.success());
        let err = stderr(&out);
        assert!(err.contains("unknown flag"));
    }
}

// ---------------------------------------------------------------------------
// tags subcommand
// ---------------------------------------------------------------------------

mod tags {
    use super::*;

    #[test]
    fn plain_output_shows_tag_counts() {
        let out = run(&["tags", &fixtures_path_str()]);
        assert!(out.status.success());
        let text = stdout(&out);
        assert!(
            text.contains("Tags (2 unique)"),
            "expected 2 unique tags, got: {text}"
        );
        assert!(text.contains("fixture"), "should list fixture tag");
        assert!(text.contains("alpha"), "should list alpha tag");
    }

    #[test]
    fn plain_output_sorted_by_count_desc() {
        let out = run(&["tags", &fixtures_path_str()]);
        let text = stdout(&out);
        // "fixture" appears on 2 pages (alpha, beta), "alpha" on 1 page.
        // fixture should appear before alpha.
        let fixture_pos = text.find("fixture").unwrap();
        let alpha_pos = text.find("alpha").unwrap();
        assert!(
            fixture_pos < alpha_pos,
            "fixture (count 2) should appear before alpha (count 1)"
        );
    }

    #[test]
    fn json_output_is_valid_array() {
        let out = run(&["tags", "--json", &fixtures_path_str()]);
        assert!(out.status.success());
        let json: serde_json::Value =
            serde_json::from_str(&stdout(&out)).expect("tags --json should produce valid JSON");
        let arr = json.as_array().expect("should be an array");
        assert_eq!(arr.len(), 2);
        // Sorted by count descending: fixture (2), alpha (1).
        assert_eq!(arr[0]["tag"], "fixture");
        assert_eq!(arr[0]["count"], 2);
        assert_eq!(arr[1]["tag"], "alpha");
        assert_eq!(arr[1]["count"], 1);
    }

    #[test]
    fn tsv_output_has_two_columns() {
        let out = run(&["tags", "--tsv", &fixtures_path_str()]);
        assert!(out.status.success());
        let text = stdout(&out);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "expected 2 TSV lines");
        for line in &lines {
            let parts: Vec<&str> = line.split('\t').collect();
            assert_eq!(parts.len(), 2, "expected tag\\tcount, got: {line}");
        }
        // First line should be fixture with count 2.
        assert!(lines[0].starts_with("fixture\t2"));
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
        let out = run(&["tags", "--for"]);
        assert!(!out.status.success());
    }

    #[test]
    fn unknown_flag_exits_nonzero() {
        let out = run(&["tags", "--badarg", &fixtures_path_str()]);
        assert!(!out.status.success());
        let err = stderr(&out);
        assert!(
            err.contains("unknown flag"),
            "expected unknown flag error, got: {err}"
        );
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
            text.contains("orphan_pages: 5"),
            "expected 5 orphan pages, got: {text}"
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
        assert!(text.contains("Orphan Pages (5)"));
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
    }

    #[test]
    fn exits_nonzero_when_issues_found() {
        let out = run(&["check", &fixtures_path_str()]);
        assert!(
            !out.status.success(),
            "should exit nonzero when orphan pages exist"
        );
    }

    #[test]
    fn json_output_has_expected_fields() {
        let out = run(&["check", "--json", &fixtures_path_str()]);
        let text = stdout(&out);
        let json: serde_json::Value =
            serde_json::from_str(&text).expect("check --json should produce valid JSON");
        assert!(json["broken_links"].is_array());
        assert!(json["orphan_pages"].is_array());
        assert!(json["load_errors"].is_array());
        assert!(json["index_issues"].is_array());
    }

    #[test]
    fn json_output_has_correct_counts() {
        let out = run(&["check", "--json", &fixtures_path_str()]);
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        let broken = json["broken_links"].as_array().unwrap();
        assert_eq!(broken.len(), 0, "expected 0 broken links");
        let orphans = json["orphan_pages"].as_array().unwrap();
        assert_eq!(orphans.len(), 5, "expected 5 orphan pages");
    }

    #[test]
    fn json_orphan_pages_have_id_and_title() {
        let out = run(&["check", "--json", &fixtures_path_str()]);
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        for page in json["orphan_pages"].as_array().unwrap() {
            assert!(page["id"].is_string(), "orphan should have id");
            assert!(page["title"].is_string(), "orphan should have title");
        }
    }

    #[test]
    fn json_orphan_excludes_linked_page() {
        let out = run(&["check", "--json", &fixtures_path_str()]);
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        let orphan_ids: Vec<&str> = json["orphan_pages"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|p| p["id"].as_str())
            .collect();
        assert!(
            !orphan_ids.contains(&"fixture-page-alpha"),
            "alpha should not be orphan (linked by beta)"
        );
        assert!(
            orphan_ids.contains(&"fixture-page-beta"),
            "beta should be orphan"
        );
    }

    #[test]
    fn unknown_flag_exits_nonzero() {
        let out = run(&["check", "--badarg", &fixtures_path_str()]);
        assert!(!out.status.success());
        let err = stderr(&out);
        assert!(
            err.contains("unknown flag"),
            "expected unknown flag error, got: {err}"
        );
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
    fn broken_links_fixture_detects_broken_links() {
        let out = run(&["check", &broken_links_path_str()]);
        let text = stdout(&out);
        assert!(
            text.contains("Broken Links (3)"),
            "expected 3 broken links, got: {text}"
        );
    }

    #[test]
    fn broken_links_fixture_plain_shows_targets() {
        let out = run(&["check", &broken_links_path_str()]);
        let text = stdout(&out);
        assert!(
            text.contains("page:bl-page-ghost"),
            "should report ghost target, got: {text}"
        );
        assert!(
            text.contains("page:bl-page-nope"),
            "should report nope target, got: {text}"
        );
        assert!(
            text.contains("page:bl-page-also-nope"),
            "should report also-nope target, got: {text}"
        );
    }

    #[test]
    fn broken_links_fixture_json_has_broken_links() {
        let out = run(&["check", "--json", &broken_links_path_str()]);
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        let broken = json["broken_links"].as_array().unwrap();
        assert_eq!(broken.len(), 3, "expected 3 broken links");
        let targets: Vec<&str> = broken.iter().filter_map(|b| b["target"].as_str()).collect();
        assert!(targets.contains(&"page:bl-page-ghost"));
        assert!(targets.contains(&"page:bl-page-nope"));
        assert!(targets.contains(&"page:bl-page-also-nope"));
    }

    #[test]
    fn broken_links_fixture_json_broken_link_has_source() {
        let out = run(&["check", "--json", &broken_links_path_str()]);
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        let broken = json["broken_links"].as_array().unwrap();
        let ghost = broken
            .iter()
            .find(|b| b["target"] == "page:bl-page-ghost")
            .expect("ghost link should be in results");
        assert_eq!(ghost["source_id"], "bl-page-ok");
        assert_eq!(ghost["source_title"], "ok page");
    }

    #[test]
    fn broken_links_fixture_orphans_correct() {
        let out = run(&["check", "--json", &broken_links_path_str()]);
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        let orphan_ids: Vec<&str> = json["orphan_pages"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|p| p["id"].as_str())
            .collect();
        // target is linked to by ok, so only ok and multi should be orphans
        assert!(
            !orphan_ids.contains(&"bl-page-target"),
            "target should not be orphan"
        );
        assert!(orphan_ids.contains(&"bl-page-ok"), "ok should be orphan");
        assert!(
            orphan_ids.contains(&"bl-page-multi"),
            "multi should be orphan"
        );
    }

    #[test]
    fn broken_links_fixture_exits_nonzero() {
        let out = run(&["check", &broken_links_path_str()]);
        assert!(
            !out.status.success(),
            "should exit nonzero when broken links exist"
        );
    }

    #[test]
    fn broken_links_fixture_json_has_load_errors_field() {
        let out = run(&["check", "--json", &broken_links_path_str()]);
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        assert!(
            json["load_errors"].is_array(),
            "load_errors should be present"
        );
        assert_eq!(
            json["load_errors"].as_array().unwrap().len(),
            0,
            "no load errors expected for valid fixture"
        );
    }

    #[test]
    fn broken_links_fixture_json_has_index_issues_field() {
        let out = run(&["check", "--json", &broken_links_path_str()]);
        let json: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        assert!(
            json["index_issues"].is_array(),
            "index_issues should be present"
        );
        assert_eq!(
            json["index_issues"].as_array().unwrap().len(),
            0,
            "no index issues expected for valid fixture"
        );
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
        assert!(!out.status.success());
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
        let out = run(&["list", "--badarg"]);
        assert_eq!(out.status.code(), Some(2));
        assert!(stderr(&out).contains("unknown flag: --badarg"));
    }
}

// ---------------------------------------------------------------------------
// export subcommand
// ---------------------------------------------------------------------------

mod export {
    use super::*;

    #[test]
    fn unknown_flag_exits_nonzero() {
        let out = run(&["export", "--badarg", &fixtures_path_str()]);
        assert!(!out.status.success());
        let err = stderr(&out);
        assert!(
            err.contains("unknown flag"),
            "expected unknown flag error, got: {err}"
        );
    }
}

// ---------------------------------------------------------------------------
// import subcommand
// ---------------------------------------------------------------------------

mod import {
    use super::*;

    #[test]
    fn unknown_flag_exits_nonzero() {
        let out = run(&["import", "--badarg", &fixtures_path_str()]);
        assert!(!out.status.success());
        let err = stderr(&out);
        assert!(
            err.contains("unknown flag"),
            "expected unknown flag error, got: {err}"
        );
    }
}

// ---------------------------------------------------------------------------
// export -> import round-trip
// ---------------------------------------------------------------------------

mod import_export {
    use super::*;

    fn unique_temp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "lepiter-cli-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

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
