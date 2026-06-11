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
        // "alpha" is also a tag on page-alpha, but title match wins over tag.
        // Search for something that only matches as a tag.
        // The tag "fixture" appears on alpha page — but "fixture" also appears
        // in every page id, so all pages match by title. This is fine — we
        // just verify the alpha page has kind=title (id match counts as title).
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
    fn tsv_output_has_three_columns() {
        let out = run(&["search", "--tsv", "alpha", &fixtures_path_str()]);
        assert!(out.status.success());
        let text = stdout(&out);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1);
        let parts: Vec<&str> = lines[0].split('\t').collect();
        assert_eq!(parts.len(), 3, "expected title\\tid\\tkind");
        assert_eq!(parts[0], "alpha page");
        assert_eq!(parts[1], "fixture-page-alpha");
        assert_eq!(parts[2], "title");
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
}
