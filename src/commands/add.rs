use crate::db::{self, commands_repo};
use crate::models::StoredCommand;
use crate::{interactive, paths, placeholder};
use anyhow::{anyhow, Result};
use chrono::Utc;
use nanoid::nanoid;

pub fn run(template: String) -> Result<()> {
    let mut conn = db::open(paths::db_path()?)?;
    if !db::is_initialized(&conn)? {
        return Err(anyhow!("inr is not initialized - run `inr i` first"));
    }

    let vars = placeholder::parse_placeholders(&template)?;

    if !vars.is_empty() {
        println!("Found {} variable(s) in this command:", vars.len());
        for (i, v) in vars.iter().enumerate() {
            println!("  {}. {} ({})", i + 1, v.name, v.kind);
        }
        if !interactive::confirm("Save this command with these variables?", true)? {
            println!("Cancelled.");
            return Ok(());
        }
    }

    let description = interactive::prompt_text("Description (searchable):")?;
    let now = Utc::now().to_rfc3339();
    let id = nanoid!(6);

    let cmd = StoredCommand {
        id: id.clone(),
        template,
        description,
        created_at: now.clone(),
        updated_at: now,
    };
    commands_repo::insert_command(&mut conn, &cmd, &vars)?;

    println!("Saved. id: {id}");
    Ok(())
}
