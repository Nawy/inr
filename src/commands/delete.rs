use crate::db::{self, commands_repo};
use crate::{interactive, paths};
use anyhow::Result;

pub fn run(id: String) -> Result<()> {
    let conn = db::open(paths::db_path()?)?;

    let Some(cmd) = commands_repo::get_command(&conn, &id)? else {
        println!("No command with id '{id}'.");
        return Ok(());
    };

    let message = format!("Delete \"{}\" (id: {})?", cmd.description, cmd.id);
    if !interactive::confirm(&message, false)? {
        println!("Cancelled.");
        return Ok(());
    }

    commands_repo::delete_command(&conn, &id)?;
    println!("Deleted.");
    Ok(())
}
