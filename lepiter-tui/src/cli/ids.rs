use anyhow::Result;

use super::{ArgSpec, open_kb, parse_args};

const SPEC: ArgSpec<'static> = ArgSpec {
    usage: "usage: lepiter-cli ids [kb-path]\n\n\
            prints page ids only, sorted by title.",
    toggles: &[],
    valued: &[],
};

pub fn run_ids(args: Vec<String>) -> Result<()> {
    let Some(args) = parse_args(args, &SPEC)? else {
        return Ok(());
    };

    let index = open_kb(&args.kb_path(0))?;
    for meta in index.sorted_pages() {
        println!("{}", meta.id);
    }
    Ok(())
}
