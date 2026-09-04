use crate::db::{self, env_repo};
use crate::{interactive, paths};
use anyhow::Result;

pub fn run(id: String) -> Result<()> {
    let conn = db::open(paths::db_path()?)?;

    let Some(env) = env_repo::get_by_id(&conn, &id)? else {
        println!("No env with id '{id}'.");
        return Ok(());
    };

    let message = format!("Delete env '{}' (id: {})?", env.name, env.id);
    if !interactive::confirm(&message, false)? {
        println!("Cancelled.");
        return Ok(());
    }

    env_repo::delete_env(&conn, &id)?;
    println!("Deleted.");
    Ok(())
}
