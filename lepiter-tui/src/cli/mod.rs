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

use anyhow::{Result, bail};
use lepiter_core::{KnowledgeBaseIndex, TitleResolution};

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
