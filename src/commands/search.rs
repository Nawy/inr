use crate::db::{self, commands_repo, history_repo};
use crate::{paths, shell, tui, vars};
use anyhow::Result;
use chrono::Utc;
use std::rc::Rc;

pub fn run() -> Result<()> {
    let conn = Rc::new(db::open(paths::db_path()?)?);

    let Some(cmd) = tui::select_command(&conn)? else {
        println!("Cancelled.");
        return Ok(());
    };

    let cmd_vars = commands_repo::get_command_variables(&conn, &cmd.id)?;

    let mut resolved = Vec::new();
    let mut keys = vars::KeyCache::new(conn.as_ref());

    for var in &cmd_vars {
        let Some(rv) = vars::resolve_variable(&conn, &mut keys, var)? else {
            println!("Cancelled.");
            return Ok(());
        };
        resolved.push(rv);
    }
    let bindings: Vec<_> = resolved.iter().map(|rv| rv.to_history_binding()).collect();

    let (command_line, secret_envs) = shell::build_command_line(&cmd.template, &resolved)?;
    let status = shell::run_inherited(&command_line, &secret_envs)?;

    history_repo::insert_history(
        &conn,
        &cmd.id,
        &cmd.template,
        &Utc::now().to_rfc3339(),
        &bindings,
    )?;

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}
