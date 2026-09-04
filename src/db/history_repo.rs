use crate::models::{HistoryBinding, HistoryEntry};
use anyhow::Result;
use rusqlite::{params, Connection};

pub fn insert_history(
    conn: &Connection,
    command_id: &str,
    template: &str,
    executed_at: &str,
    bindings: &[HistoryBinding],
) -> Result<()> {
    let bindings_json = serde_json::to_string(bindings)?;
    conn.execute(
        "INSERT INTO history (command_id, template, executed_at, bindings) VALUES (?1, ?2, ?3, ?4)",
        params![command_id, template, executed_at, bindings_json],
    )?;
    Ok(())
}

pub fn list_recent(conn: &Connection, limit: i64) -> Result<Vec<HistoryEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, command_id, template, executed_at, bindings FROM history ORDER BY id DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit], |row| {
        let bindings_json: String = row.get(4)?;
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            bindings_json,
        ))
    })?;

    let mut out = Vec::new();
    for row in rows {
        let (id, command_id, template, executed_at, bindings_json) = row?;
        let bindings: Vec<HistoryBinding> = serde_json::from_str(&bindings_json)?;
        out.push(HistoryEntry {
            id,
            command_id,
            template,
            executed_at,
            bindings,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    #[test]
    fn insert_and_list_roundtrip_never_leaks_secret_value() {
        let conn = open_in_memory().unwrap();
        let bindings = vec![
            HistoryBinding {
                name: "host".into(),
                kind: "secret".into(),
                display: "@prod-server".into(),
            },
            HistoryBinding {
                name: "message".into(),
                kind: "text".into(),
                display: "hello team".into(),
            },
        ];
        insert_history(&conn, "cmd1", "ssh %s:host", "2026-01-01T00:00:00Z", &bindings).unwrap();

        let entries = list_recent(&conn, 10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].bindings.len(), 2);
        assert_eq!(entries[0].bindings[0].display, "@prod-server");
    }

    #[test]
    fn list_recent_orders_newest_first() {
        let conn = open_in_memory().unwrap();
        insert_history(&conn, "cmd1", "echo a", "2026-01-01T00:00:00Z", &[]).unwrap();
        insert_history(&conn, "cmd2", "echo b", "2026-01-02T00:00:00Z", &[]).unwrap();

        let entries = list_recent(&conn, 10).unwrap();
        assert_eq!(entries[0].command_id, "cmd2");
        assert_eq!(entries[1].command_id, "cmd1");
    }
}
