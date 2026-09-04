use crate::db::env_repo;
use crate::models::{CmdVarKind, CmdVariable, StoredCommand, StoredCommandVariable};
use anyhow::Result;
use rusqlite::{params, Connection, Row};
use std::collections::BTreeMap;

fn map_row(row: &Row) -> rusqlite::Result<StoredCommand> {
    Ok(StoredCommand {
        id: row.get(0)?,
        template: row.get(1)?,
        description: row.get(2)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
    })
}

pub fn insert_command(
    conn: &mut Connection,
    cmd: &StoredCommand,
    vars: &[CmdVariable],
) -> Result<()> {
    let tx = conn.transaction()?;
    insert_command_raw(&tx, cmd, vars)?;
    tx.commit()?;
    Ok(())
}

/// Same as `insert_command`, but runs directly against `conn` without
/// opening its own transaction - for callers (like `inr import`) that need
/// to batch many command/env inserts and deletes into one outer
/// transaction.
pub fn insert_command_raw(
    conn: &Connection,
    cmd: &StoredCommand,
    vars: &[CmdVariable],
) -> Result<()> {
    conn.execute(
        "INSERT INTO commands (id, template, description, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![cmd.id, cmd.template, cmd.description, cmd.created_at, cmd.updated_at],
    )?;
    set_command_variables(conn, &cmd.id, vars, &[])
}

pub fn get_command(conn: &Connection, id: &str) -> Result<Option<StoredCommand>> {
    let result = conn.query_row(
        "SELECT id, template, description, created_at, updated_at FROM commands WHERE id = ?1",
        params![id],
        map_row,
    );
    match result {
        Ok(cmd) => Ok(Some(cmd)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Finds a command by its exact template text - used by `inr import` to
/// decide whether an incoming command already exists locally (commands have
/// no other natural key: `id` is a random nanoid, independently generated
/// on each machine).
pub fn find_by_template(conn: &Connection, template: &str) -> Result<Option<StoredCommand>> {
    let result = conn.query_row(
        "SELECT id, template, description, created_at, updated_at FROM commands WHERE template = ?1",
        params![template],
        map_row,
    );
    match result {
        Ok(cmd) => Ok(Some(cmd)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Returns every stored command, ordered by id. Used by `inr export` to
/// bundle the full set of commands into a transfer file.
pub fn list_all(conn: &Connection) -> Result<Vec<StoredCommand>> {
    let mut stmt = conn.prepare(
        "SELECT id, template, description, created_at, updated_at FROM commands ORDER BY id ASC",
    )?;
    let rows = stmt.query_map([], map_row)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
}

pub fn get_command_variables(conn: &Connection, command_id: &str) -> Result<Vec<StoredCommandVariable>> {
    let mut stmt = conn.prepare(
        "SELECT kind, name, default_env_id FROM command_variables WHERE command_id = ?1 ORDER BY position ASC",
    )?;
    let rows = stmt.query_map(params![command_id], |row| {
        let kind_str: String = row.get(0)?;
        let name: String = row.get(1)?;
        let default_env_id: Option<String> = row.get(2)?;
        Ok((kind_str, name, default_env_id))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (kind_str, name, default_env_id) = row?;
        let kind = CmdVarKind::from_str(&kind_str)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
        out.push(StoredCommandVariable {
            var: CmdVariable { kind, name },
            default_env_id,
        });
    }
    Ok(out)
}

/// Replaces every stored variable for `command_id` with `new_vars`, in
/// order (so `position` always matches the template's current variable
/// order). A variable in `new_vars` whose (name, kind) matches one in
/// `previous` keeps that variable's `default_env_id`; anything else - a
/// brand new variable, or one whose kind changed - starts with no default.
/// Used both for a fresh command (`previous` empty) and for `inr e`
/// re-saving an edited template.
pub fn set_command_variables(
    conn: &Connection,
    command_id: &str,
    new_vars: &[CmdVariable],
    previous: &[StoredCommandVariable],
) -> Result<()> {
    conn.execute(
        "DELETE FROM command_variables WHERE command_id = ?1",
        params![command_id],
    )?;
    for (i, v) in new_vars.iter().enumerate() {
        let default_env_id = previous
            .iter()
            .find(|p| p.var.name == v.name && p.var.kind == v.kind)
            .and_then(|p| p.default_env_id.clone());
        conn.execute(
            "INSERT INTO command_variables (command_id, position, kind, name, default_env_id) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![command_id, i as i64, v.kind.as_str(), v.name, default_env_id],
        )?;
    }
    Ok(())
}

/// Sets (or, with `None`, clears) one variable's default env by name.
pub fn set_default_env(
    conn: &Connection,
    command_id: &str,
    var_name: &str,
    default_env_id: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE command_variables SET default_env_id = ?1 WHERE command_id = ?2 AND name = ?3",
        params![default_env_id, command_id, var_name],
    )?;
    Ok(())
}

/// Updates a command's editable fields (used by `inr e`). Variables are
/// updated separately via `set_command_variables` - the FTS trigger
/// `commands_au` reindexes `template`/`description` automatically.
pub fn update_command(
    conn: &Connection,
    id: &str,
    template: &str,
    description: &str,
    updated_at: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE commands SET template = ?1, description = ?2, updated_at = ?3 WHERE id = ?4",
        params![template, description, updated_at, id],
    )?;
    Ok(())
}

/// Updates a command's fields and its full variable set together, in one
/// transaction (used by `inr e`) - matches `insert_command`'s transactional
/// pattern, so a failure partway through never leaves `commands.template`
/// updated while `command_variables` is stale or partially rebuilt.
pub fn update_command_and_variables(
    conn: &mut Connection,
    id: &str,
    template: &str,
    description: &str,
    updated_at: &str,
    new_vars: &[CmdVariable],
    previous: &[StoredCommandVariable],
) -> Result<()> {
    let tx = conn.transaction()?;
    update_command(&tx, id, template, description, updated_at)?;
    set_command_variables(&tx, id, new_vars, previous)?;
    tx.commit()?;
    Ok(())
}

/// Maps each variable name that has a default env to that env's *name*,
/// skipping any default whose env id no longer resolves to a real env.
/// Used by `inr export` - export carries default envs by name (see
/// `transfer::ExportedCommand::variable_defaults`), never by id, since ids
/// are only meaningful within the machine that generated them.
pub fn get_variable_default_names(
    conn: &Connection,
    command_id: &str,
) -> Result<BTreeMap<String, String>> {
    let vars = get_command_variables(conn, command_id)?;
    let mut out = BTreeMap::new();
    for v in vars {
        // Nested (not collapsed via a let-chain) to keep this compiling on
        // the documented rustc 1.85+ floor - let-chains need 1.88+.
        #[allow(clippy::collapsible_if)]
        if let Some(env_id) = v.default_env_id {
            if let Some(env) = env_repo::get_by_id(conn, &env_id)? {
                out.insert(v.var.name, env.name);
            }
        }
    }
    Ok(out)
}

/// Deletes a command by id. Returns whether a row was actually deleted.
/// `command_variables` cascade-deletes via the FK (foreign_keys pragma is
/// enabled on every connection opened through `db::open`).
pub fn delete_command(conn: &Connection, id: &str) -> Result<bool> {
    let affected = conn.execute("DELETE FROM commands WHERE id = ?1", params![id])?;
    Ok(affected > 0)
}

/// Escapes a raw user query into an FTS5 MATCH expression: each
/// whitespace-separated token becomes a quoted prefix-match term, so
/// arbitrary input (including FTS5 syntax characters) can never produce an
/// invalid or unintended query.
pub fn build_fts_query(raw: &str) -> String {
    raw.split_whitespace()
        .map(|tok| format!("\"{}\"*", tok.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Searches commands by description + template text. Empty query returns
/// the most recently updated commands instead of an FTS5 match.
pub fn search_commands(conn: &Connection, raw_query: &str, limit: i64) -> Result<Vec<StoredCommand>> {
    if raw_query.trim().is_empty() {
        let mut stmt = conn.prepare(
            "SELECT id, template, description, created_at, updated_at FROM commands ORDER BY updated_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], map_row)?;
        return rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into);
    }

    let fts_query = build_fts_query(raw_query);
    let mut stmt = conn.prepare(
        "SELECT c.id, c.template, c.description, c.created_at, c.updated_at
         FROM commands_fts f
         JOIN commands c ON c.rowid = f.rowid
         WHERE commands_fts MATCH ?1
         ORDER BY f.rank
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![fts_query, limit], map_row)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    fn sample(id: &str, template: &str, description: &str) -> StoredCommand {
        StoredCommand {
            id: id.to_string(),
            template: template.to_string(),
            description: description.to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn insert_and_get_roundtrip_with_variables() {
        let mut conn = open_in_memory().unwrap();
        let vars = vec![CmdVariable {
            kind: CmdVarKind::Secret,
            name: "host".to_string(),
        }];
        insert_command(&mut conn, &sample("id1", "ssh %s:host", "connect to server"), &vars)
            .unwrap();

        let fetched = get_command(&conn, "id1").unwrap().unwrap();
        assert_eq!(fetched.template, "ssh %s:host");

        let fetched_vars = get_command_variables(&conn, "id1").unwrap();
        assert_eq!(fetched_vars.len(), 1);
        assert_eq!(fetched_vars[0].var, vars[0]);
        assert_eq!(fetched_vars[0].default_env_id, None);
    }

    #[test]
    fn set_default_env_updates_only_the_named_variable() {
        let mut conn = open_in_memory().unwrap();
        let vars = vec![
            CmdVariable { kind: CmdVarKind::Secret, name: "host".to_string() },
            CmdVariable { kind: CmdVarKind::Text, name: "user".to_string() },
        ];
        insert_command(&mut conn, &sample("id1", "ssh %s:host@%t:user", "ssh"), &vars).unwrap();

        set_default_env(&conn, "id1", "host", Some("env1")).unwrap();

        let fetched = get_command_variables(&conn, "id1").unwrap();
        let host = fetched.iter().find(|v| v.var.name == "host").unwrap();
        let user = fetched.iter().find(|v| v.var.name == "user").unwrap();
        assert_eq!(host.default_env_id, Some("env1".to_string()));
        assert_eq!(user.default_env_id, None);

        set_default_env(&conn, "id1", "host", None).unwrap();
        let fetched = get_command_variables(&conn, "id1").unwrap();
        assert_eq!(fetched.iter().find(|v| v.var.name == "host").unwrap().default_env_id, None);
    }

    #[test]
    fn set_command_variables_carries_default_when_name_and_kind_match() {
        let mut conn = open_in_memory().unwrap();
        let vars = vec![CmdVariable { kind: CmdVarKind::Text, name: "host".to_string() }];
        insert_command(&mut conn, &sample("id1", "ping %t:host", "ping"), &vars).unwrap();
        set_default_env(&conn, "id1", "host", Some("env1")).unwrap();

        let previous = get_command_variables(&conn, "id1").unwrap();
        let new_vars = vec![
            CmdVariable { kind: CmdVarKind::Text, name: "host".to_string() },
            CmdVariable { kind: CmdVarKind::Number, name: "count".to_string() },
        ];
        set_command_variables(&conn, "id1", &new_vars, &previous).unwrap();

        let fetched = get_command_variables(&conn, "id1").unwrap();
        assert_eq!(fetched.len(), 2);
        assert_eq!(fetched[0].var.name, "host");
        assert_eq!(fetched[0].default_env_id, Some("env1".to_string()));
        assert_eq!(fetched[1].var.name, "count");
        assert_eq!(fetched[1].default_env_id, None);
    }

    #[test]
    fn set_command_variables_drops_default_when_kind_changes() {
        let mut conn = open_in_memory().unwrap();
        let vars = vec![CmdVariable { kind: CmdVarKind::Text, name: "count".to_string() }];
        insert_command(&mut conn, &sample("id1", "echo %t:count", "echo"), &vars).unwrap();
        set_default_env(&conn, "id1", "count", Some("env1")).unwrap();

        let previous = get_command_variables(&conn, "id1").unwrap();
        let new_vars = vec![CmdVariable { kind: CmdVarKind::Number, name: "count".to_string() }];
        set_command_variables(&conn, "id1", &new_vars, &previous).unwrap();

        let fetched = get_command_variables(&conn, "id1").unwrap();
        assert_eq!(fetched[0].default_env_id, None);
    }

    #[test]
    fn update_command_and_variables_updates_both_in_one_transaction() {
        let mut conn = open_in_memory().unwrap();
        let vars = vec![
            CmdVariable { kind: CmdVarKind::Text, name: "host".to_string() },
            CmdVariable { kind: CmdVarKind::Number, name: "port".to_string() },
        ];
        insert_command(&mut conn, &sample("id1", "ping %t:host %n:port", "ping"), &vars).unwrap();
        set_default_env(&conn, "id1", "host", Some("env1")).unwrap();

        let previous = get_command_variables(&conn, "id1").unwrap();
        // "port" is dropped, "host" is kept (and should carry its default
        // env forward), "count" is a brand new variable.
        let new_vars = vec![
            CmdVariable { kind: CmdVarKind::Text, name: "host".to_string() },
            CmdVariable { kind: CmdVarKind::Number, name: "count".to_string() },
        ];

        update_command_and_variables(
            &mut conn,
            "id1",
            "ping %t:host %n:count",
            "ping new",
            "2026-02-01T00:00:00Z",
            &new_vars,
            &previous,
        )
        .unwrap();

        let fetched_cmd = get_command(&conn, "id1").unwrap().unwrap();
        assert_eq!(fetched_cmd.template, "ping %t:host %n:count");
        assert_eq!(fetched_cmd.description, "ping new");
        assert_eq!(fetched_cmd.updated_at, "2026-02-01T00:00:00Z");

        let fetched_vars = get_command_variables(&conn, "id1").unwrap();
        assert_eq!(fetched_vars.len(), 2);
        let host = fetched_vars.iter().find(|v| v.var.name == "host").unwrap();
        assert_eq!(host.default_env_id, Some("env1".to_string()));
        let count = fetched_vars.iter().find(|v| v.var.name == "count").unwrap();
        assert_eq!(count.default_env_id, None);
    }

    #[test]
    fn update_command_changes_template_and_description() {
        let mut conn = open_in_memory().unwrap();
        insert_command(&mut conn, &sample("id1", "echo old", "old desc"), &[]).unwrap();

        update_command(&conn, "id1", "echo new", "new desc", "2026-02-01T00:00:00Z").unwrap();

        let fetched = get_command(&conn, "id1").unwrap().unwrap();
        assert_eq!(fetched.template, "echo new");
        assert_eq!(fetched.description, "new desc");
        assert_eq!(fetched.updated_at, "2026-02-01T00:00:00Z");
    }

    #[test]
    fn get_variable_default_names_only_includes_resolvable_defaults() {
        let mut conn = open_in_memory().unwrap();
        let vars = vec![
            CmdVariable { kind: CmdVarKind::Text, name: "host".to_string() },
            CmdVariable { kind: CmdVarKind::Text, name: "user".to_string() },
        ];
        insert_command(&mut conn, &sample("id1", "ssh %t:host@%t:user", "ssh"), &vars).unwrap();

        let env = crate::models::StoredEnv {
            id: "env1".to_string(),
            name: "myhost".to_string(),
            kind: crate::models::EnvKind::Text,
            value: b"example.com".to_vec(),
            nonce: None,
            description: String::new(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        };
        crate::db::env_repo::insert_env(&conn, &env).unwrap();

        set_default_env(&conn, "id1", "host", Some("env1")).unwrap();
        // "user" points at an id that doesn't exist - must be skipped, not error.
        set_default_env(&conn, "id1", "user", Some("does-not-exist")).unwrap();

        let names = get_variable_default_names(&conn, "id1").unwrap();
        assert_eq!(names.len(), 1);
        assert_eq!(names.get("host"), Some(&"myhost".to_string()));
    }

    #[test]
    fn delete_removes_command_and_cascades_variables() {
        let mut conn = open_in_memory().unwrap();
        let vars = vec![CmdVariable {
            kind: CmdVarKind::Text,
            name: "msg".to_string(),
        }];
        insert_command(&mut conn, &sample("id1", "echo %t:msg", "say something"), &vars).unwrap();

        assert!(delete_command(&conn, "id1").unwrap());
        assert!(get_command(&conn, "id1").unwrap().is_none());
        assert!(get_command_variables(&conn, "id1").unwrap().is_empty());
    }

    #[test]
    fn find_by_template_matches_exact_text() {
        let mut conn = open_in_memory().unwrap();
        insert_command(&mut conn, &sample("id1", "ssh %s:host", "connect"), &[]).unwrap();

        assert_eq!(
            find_by_template(&conn, "ssh %s:host").unwrap().unwrap().id,
            "id1"
        );
        assert!(find_by_template(&conn, "ssh %s:other").unwrap().is_none());
    }

    #[test]
    fn list_all_returns_every_command() {
        let mut conn = open_in_memory().unwrap();
        insert_command(&mut conn, &sample("id1", "echo a", "a"), &[]).unwrap();
        insert_command(&mut conn, &sample("id2", "echo b", "b"), &[]).unwrap();

        let all = list_all(&conn).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn delete_nonexistent_returns_false() {
        let conn = open_in_memory().unwrap();
        assert!(!delete_command(&conn, "nope").unwrap());
    }

    #[test]
    fn search_matches_description_and_template() {
        let mut conn = open_in_memory().unwrap();
        insert_command(
            &mut conn,
            &sample("id1", "docker compose up -d", "restart the dev stack"),
            &[],
        )
        .unwrap();
        insert_command(&mut conn, &sample("id2", "ls -la", "list files"), &[]).unwrap();

        let by_description = search_commands(&conn, "restart", 10).unwrap();
        assert_eq!(by_description.len(), 1);
        assert_eq!(by_description[0].id, "id1");

        let by_template = search_commands(&conn, "docker", 10).unwrap();
        assert_eq!(by_template.len(), 1);
        assert_eq!(by_template[0].id, "id1");
    }

    #[test]
    fn search_handles_fts5_special_characters_without_error() {
        let mut conn = open_in_memory().unwrap();
        insert_command(&mut conn, &sample("id1", "echo hi", "test \"quoted\""), &[]).unwrap();

        // characters like " * ( ) that have FTS5 syntax meaning must not
        // cause a query error - build_fts_query escapes/quotes them
        let result = search_commands(&conn, "\"weird* (query)", 10);
        assert!(result.is_ok());
    }

    #[test]
    fn empty_query_returns_recent_commands() {
        let mut conn = open_in_memory().unwrap();
        insert_command(&mut conn, &sample("id1", "echo hi", "greet"), &[]).unwrap();
        let results = search_commands(&conn, "", 10).unwrap();
        assert_eq!(results.len(), 1);
    }
}
