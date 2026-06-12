//! core data model and parser for lepiter knowledge bases stored as page json files.
//!
//! # scope
//! - scans a lepiter directory and builds a metadata index keyed by page id.
//! - loads and parses individual pages lazily by id.
//! - converts page snippet trees into a stable block-oriented node model.
//! - preserves unknown node types as [`Node::Unknown`] to keep consumers resilient.
//! - provides a plugin sdk for external snippet renderers (`plugin` module).
//!
//! # example
//! ```no_run
//! use lepiter_core::KnowledgeBase;
//!
//! # fn main() -> anyhow::Result<()> {
//! let index = KnowledgeBase::open("./lepiter")?;
//! for page in index.sorted_pages() {
//!     println!("{} - {}", page.id, page.title);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # plugin sdk
//! ```no_run
//! use lepiter_core::plugin::{PluginRequest, PluginResponse};
//! use lepiter_core::lepiter_plugin_main;
//!
//! fn handle(req: PluginRequest) -> PluginResponse {
//!     if req.typ != "wardleyMap" {
//!         return PluginResponse::error("unsupported type");
//!     }
//!     PluginResponse::ok(vec!["example".to_string()])
//! }
//!
//! lepiter_plugin_main!(handle);
//! ```

mod index;
mod model;
mod parse;
mod render;
mod util;

pub mod plugin;

pub use index::{KnowledgeBase, KnowledgeBaseIndex, LinkEdge, LinkGraph};
pub use model::{
    AttachmentError, AttachmentResolver, LinkTargetKind, Node, Page, PageId, PageMeta, ParseIssue,
    ResolvedAttachment, SearchHit, SearchMatchKind, TitleResolution,
};
pub use parse::{
    collect_node_types_in_file, extract_type, is_code_snippet, parse_heading, parse_node_from_raw,
};
pub use render::{
    normalize_text, page_content_contains, render_nodes_to_text, render_page_to_text,
};
pub use util::extract_link_targets;

#[macro_export]
macro_rules! lepiter_plugin_main {
    ($handler:path) => {
        fn main() -> std::io::Result<()> {
            $crate::plugin::plugin_loop($handler)
        }
    };
}
