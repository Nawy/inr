use crate::db::{self, history_repo};
use crate::paths;
use anyhow::Result;

pub fn run() -> Result<()> {
    let conn = db::open(paths::db_path()?)?;
    let entries = history_repo::list_recent(&conn, 20)?;

    if entries.is_empty() {
        println!("No history yet.");
        return Ok(());
    }

    for entry in entries {
        println!(
            "#{}  {}  {}  [{}]",
            entry.id, entry.executed_at, entry.template, entry.command_id
        );
        for binding in entry.bindings {
            println!("    {} = {}", binding.name, binding.display);
        }
    }
    Ok(())
}
