mod check;
mod export;
mod format;
mod ids;
mod import;
mod info;
mod links;
mod list;
mod search;
mod show;
mod tags;

pub use check::run_check;
pub use export::run_export;
pub use ids::run_ids;
pub use import::run_import;
pub use info::{print_kb_info, run_info};
pub use links::run_links;
pub use list::run_list;
pub use search::run_search;
pub use show::run_show;
pub use tags::run_tags;

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use lepiter_core::{KnowledgeBase, KnowledgeBaseIndex, TitleResolution};

/// Knowledge base location used when a subcommand is given no path positional.
const DEFAULT_KB_PATH: &str = "./lepiter";

/// An argument-usage failure, such as an unrecognized flag.
///
/// `main` reports these with exit status 2; every other error exits 1.
#[derive(Debug)]
pub struct UsageError(String);

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UsageError {}

/// The flags one subcommand accepts, plus the text `-h`/`--help` prints.
struct ArgSpec<'a> {
    usage: &'a str,
    /// Standalone flags such as `--json`. Aliases are listed as separate entries.
    toggles: &'a [&'a str],
    /// Flags that consume the following argument, paired with the noun used in
    /// the missing-value error: `("--for", "tag")` reads `--for requires a tag
    /// argument`.
    valued: &'a [(&'a str, &'a str)],
}

/// The outcome of parsing one subcommand's arguments against an [`ArgSpec`].
#[derive(Debug, Default)]
struct ParsedArgs {
    toggles: Vec<String>,
    values: HashMap<String, String>,
    positional: Vec<String>,
}

impl ParsedArgs {
    fn has(&self, flag: &str) -> bool {
        self.toggles.iter().any(|seen| seen == flag)
    }

    fn value(&self, flag: &str) -> Option<&str> {
        self.values.get(flag).map(String::as_str)
    }

    fn positional(&self, index: usize) -> Option<&str> {
        self.positional.get(index).map(String::as_str)
    }

    /// The knowledge base path given at `index`, or the default location.
    fn kb_path(&self, index: usize) -> PathBuf {
        self.positional(index)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_KB_PATH))
    }
}

/// Parses one subcommand's arguments against `spec`.
///
/// Returns `Ok(None)` once `-h`/`--help` has printed the usage text, so callers
/// can stop with success. Positionals keep their command-line order; the first
/// one wins wherever a subcommand expects a single value.
fn parse_args(args: Vec<String>, spec: &ArgSpec<'_>) -> Result<Option<ParsedArgs>> {
    let mut parsed = ParsedArgs::default();
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        if arg == "-h" || arg == "--help" {
            eprintln!("{}", spec.usage);
            return Ok(None);
        }
        if spec.toggles.contains(&arg.as_str()) {
            parsed.toggles.push(arg);
        } else if let Some((_, noun)) = spec.valued.iter().find(|(flag, _)| *flag == arg) {
            let value = iter
                .next()
                .ok_or_else(|| anyhow!("{arg} requires a {noun} argument"))?;
            parsed.values.insert(arg, value);
        } else if arg.starts_with('-') {
            return Err(UsageError(format!("unknown flag: {arg}")).into());
        } else {
            parsed.positional.push(arg);
        }
    }

    Ok(Some(parsed))
}

/// Opens the knowledge base at `path`, adding the shared failure context.
fn open_kb(path: &Path) -> Result<KnowledgeBaseIndex> {
    KnowledgeBase::open(path)
        .with_context(|| format!("failed to open knowledge base at {}", path.display()))
}

fn resolve_page_id_by_title(index: &KnowledgeBaseIndex, title: &str) -> Result<String> {
    match index.resolve_page_id_by_title(title) {
        TitleResolution::Unique(id) => Ok(id),
        TitleResolution::NotFound => bail!("no page found with title matching `{title}`"),
        TitleResolution::Ambiguous(ids) => {
            let sample = ids
                .iter()
                .take(10)
                .map(|id| {
                    if let Some(meta) = index.pages.get(id) {
                        format!("{} ({})", meta.title, meta.id)
                    } else {
                        id.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            bail!("title match is ambiguous ({} matches): {sample}", ids.len())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: ArgSpec<'static> = ArgSpec {
        usage: "usage: lepiter-cli demo [--json] [--for <tag>] [kb-path]",
        toggles: &["--json", "--tsv"],
        valued: &[("--for", "tag")],
    };

    fn parse(args: &[&str]) -> Result<Option<ParsedArgs>> {
        parse_args(args.iter().map(|a| a.to_string()).collect(), &SPEC)
    }

    #[test]
    fn collects_toggles_values_and_positionals() {
        let parsed = parse(&["--json", "--for", "rust", "kb", "extra"])
            .unwrap()
            .unwrap();
        assert!(parsed.has("--json"));
        assert!(!parsed.has("--tsv"));
        assert_eq!(parsed.value("--for"), Some("rust"));
        assert_eq!(parsed.positional(0), Some("kb"));
        assert_eq!(parsed.positional(1), Some("extra"));
        assert_eq!(parsed.positional(2), None);
    }

    #[test]
    fn positionals_keep_command_line_order() {
        let parsed = parse(&["a", "--json", "b"]).unwrap().unwrap();
        assert_eq!(parsed.positional(0), Some("a"));
        assert_eq!(parsed.positional(1), Some("b"));
    }

    #[test]
    fn kb_path_falls_back_to_default() {
        let parsed = parse(&["--json"]).unwrap().unwrap();
        assert_eq!(parsed.kb_path(0), PathBuf::from(DEFAULT_KB_PATH));
        let parsed = parse(&["somewhere"]).unwrap().unwrap();
        assert_eq!(parsed.kb_path(0), PathBuf::from("somewhere"));
    }

    #[test]
    fn unknown_flag_is_a_usage_error() {
        let err = parse(&["--nope"]).unwrap_err();
        assert!(err.downcast_ref::<UsageError>().is_some());
        assert_eq!(err.to_string(), "unknown flag: --nope");
    }

    #[test]
    fn valued_flag_without_value_names_the_expected_noun() {
        let err = parse(&["--for"]).unwrap_err();
        assert!(err.downcast_ref::<UsageError>().is_none());
        assert_eq!(err.to_string(), "--for requires a tag argument");
    }

    #[test]
    fn valued_flag_takes_the_next_argument_verbatim() {
        let parsed = parse(&["--for", "--json"]).unwrap().unwrap();
        assert_eq!(parsed.value("--for"), Some("--json"));
        assert!(!parsed.has("--json"));
    }

    #[test]
    fn help_flags_stop_parsing() {
        assert!(parse(&["--help"]).unwrap().is_none());
        assert!(parse(&["-h"]).unwrap().is_none());
        assert!(parse(&["kb", "--help"]).unwrap().is_none());
    }

    #[test]
    fn help_wins_over_a_later_unknown_flag() {
        assert!(parse(&["--help", "--nope"]).unwrap().is_none());
    }
}
