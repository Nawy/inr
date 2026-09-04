use crate::models::{CmdVarKind, CmdVariable, StoredCommand};
use anyhow::Result;
use rusqlite::{params, Connection, Row};

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
    for (i, v) in vars.iter().enumerate() {
        conn.execute(
            "INSERT INTO command_variables (command_id, position, kind, name) VALUES (?1, ?2, ?3, ?4)",
            params![cmd.id, i as i64, v.kind.as_str(), v.name],
        )?;
    }
    Ok(())
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

pub fn get_command_variables(conn: &Connection, command_id: &str) -> Result<Vec<CmdVariable>> {
    let mut stmt = conn.prepare(
        "SELECT kind, name FROM command_variables WHERE command_id = ?1 ORDER BY position ASC",
    )?;
    let rows = stmt.query_map(params![command_id], |row| {
        let kind_str: String = row.get(0)?;
        let name: String = row.get(1)?;
        Ok((kind_str, name))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (kind_str, name) = row?;
        let kind = CmdVarKind::from_str(&kind_str)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
        out.push(CmdVariable { kind, name });
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
        assert_eq!(fetched_vars, vars);
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
