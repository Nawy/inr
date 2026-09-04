use crate::db::{self, commands_repo, env_repo};
use crate::models::{diff_variables, StoredEnv, VariableDiff};
use crate::{interactive, paths, placeholder, vars};
use anyhow::Result;
use chrono::Utc;
use inquire::{InquireError, Text};
use rusqlite::Connection;
use std::collections::HashMap;
use std::rc::Rc;

pub fn run(id: String) -> Result<()> {
    let mut conn = db::open(paths::db_path()?)?;

    let Some(cmd) = commands_repo::get_command(&conn, &id)? else {
        println!("No command with id '{id}'.");
        return Ok(());
    };

    let Some(new_description) = prompt_editable("Description (searchable):", &cmd.description)? else {
        println!("Cancelled.");
        return Ok(());
    };

    let Some(new_template) = prompt_new_template(&cmd.template)? else {
        println!("Cancelled.");
        return Ok(());
    };

    let old_vars = commands_repo::get_command_variables(&conn, &id)?;
    let new_vars = placeholder::parse_placeholders(&new_template)?;

    if new_template != cmd.template {
        let diff = diff_variables(&old_vars, &new_vars);
        if !diff.is_unchanged() {
            print_variable_diff(&diff);
            if !interactive::confirm("Save these changes?", true)? {
                println!("Cancelled.");
                return Ok(());
            }
        }
    }

    let now = Utc::now().to_rfc3339();
    commands_repo::update_command_and_variables(
        &mut conn, &id, &new_template, &new_description, &now, &new_vars, &old_vars,
    )?;
    println!("Saved.");

    if !new_vars.is_empty() {
        let current_defaults = load_current_defaults(&conn, &id)?;
        let conn = Rc::new(conn);
        vars::review_default_envs(&conn, &id, &new_vars, &current_defaults)?;
    }
    Ok(())
}

/// A `Text` prompt pre-filled with `current`, editable in place. Returns
/// `None` if the user cancelled (Esc).
fn prompt_editable(message: &str, current: &str) -> Result<Option<String>> {
    match Text::new(message).with_initial_value(current).prompt() {
        Ok(v) => Ok(Some(v)),
        Err(InquireError::OperationCanceled) | Err(InquireError::OperationInterrupted) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Prompts for a new template, re-prompting (keeping whatever was typed)
/// until it parses - a command's placeholders must always be well-formed
/// before it can be saved. Returns `None` if cancelled.
fn prompt_new_template(current: &str) -> Result<Option<String>> {
    let mut initial = current.to_string();
    loop {
        let Some(answer) = prompt_editable("Command:", &initial)? else {
            return Ok(None);
        };
        match placeholder::parse_placeholders(&answer) {
            Ok(_) => return Ok(Some(answer)),
            Err(e) => {
                println!("{e} - try again.");
                initial = answer;
            }
        }
    }
}

fn print_variable_diff(diff: &VariableDiff) {
    println!("This changes the command's variables:");
    for v in &diff.added {
        println!("  + {} ({})", v.name, v.kind);
    }
    for v in &diff.removed {
        let note = if v.default_env_id.is_some() {
            " - default env setting will be lost"
        } else {
            ""
        };
        println!("  - {} ({}){note}", v.var.name, v.var.kind);
    }
}

/// Builds variable-name -> currently-set (and still-existing) default env,
/// for `vars::review_default_envs` to show as each variable's starting
/// point.
fn load_current_defaults(conn: &Connection, command_id: &str) -> Result<HashMap<String, StoredEnv>> {
    let vars = commands_repo::get_command_variables(conn, command_id)?;
    let mut out = HashMap::new();
    for v in vars {
        // Nested (not collapsed via a let-chain) to keep this compiling on
        // the documented rustc 1.85+ floor - let-chains need 1.88+.
        #[allow(clippy::collapsible_if)]
        if let Some(env_id) = v.default_env_id {
            if let Some(env) = env_repo::get_by_id(conn, &env_id)? {
                out.insert(v.var.name, env);
            }
        }
    }
    Ok(out)
}
