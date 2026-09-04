use crate::db::commands_repo::build_fts_query;
use crate::models::{EnvKind, StoredEnv};
use anyhow::Result;
use rusqlite::{params, Connection, Row};

fn map_row(row: &Row) -> rusqlite::Result<StoredEnv> {
    let kind_str: String = row.get(2)?;
    let kind = EnvKind::from_str(&kind_str)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
    Ok(StoredEnv {
        id: row.get(0)?,
        name: row.get(1)?,
        kind,
        value: row.get(3)?,
        nonce: row.get(4)?,
        description: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

const SELECT_COLUMNS: &str = "id, name, kind, value, nonce, description, created_at, updated_at";
const SELECT_COLUMNS_QUALIFIED: &str =
    "e.id, e.name, e.kind, e.value, e.nonce, e.description, e.created_at, e.updated_at";

pub fn insert_env(conn: &Connection, env: &StoredEnv) -> Result<()> {
    conn.execute(
        &format!(
            "INSERT INTO envs ({SELECT_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
        ),
        params![
            env.id,
            env.name,
            env.kind.as_str(),
            env.value,
            env.nonce,
            env.description,
            env.created_at,
            env.updated_at
        ],
    )?;
    Ok(())
}

pub fn find_by_name(conn: &Connection, name: &str) -> Result<Option<StoredEnv>> {
    let result = conn.query_row(
        &format!("SELECT {SELECT_COLUMNS} FROM envs WHERE name = ?1"),
        params![name],
        map_row,
    );
    match result {
        Ok(env) => Ok(Some(env)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn get_by_id(conn: &Connection, id: &str) -> Result<Option<StoredEnv>> {
    let result = conn.query_row(
        &format!("SELECT {SELECT_COLUMNS} FROM envs WHERE id = ?1"),
        params![id],
        map_row,
    );
    match result {
        Ok(env) => Ok(Some(env)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Returns every stored env, ordered by id. Used by `inr export` to bundle
/// the full set of envs into a transfer file.
pub fn list_all(conn: &Connection) -> Result<Vec<StoredEnv>> {
    let mut stmt = conn.prepare(&format!("SELECT {SELECT_COLUMNS} FROM envs ORDER BY id ASC"))?;
    let rows = stmt.query_map([], map_row)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
}

/// Deletes an env by id. Returns whether a row was actually deleted.
pub fn delete_env(conn: &Connection, id: &str) -> Result<bool> {
    let affected = conn.execute("DELETE FROM envs WHERE id = ?1", params![id])?;
    Ok(affected > 0)
}

/// Searches envs by name + description. Empty query returns the most
/// recently updated envs instead of an FTS5 match. `kind_filter`, when
/// non-empty, restricts results to those kinds - used by the `@` lookup
/// inside a command variable prompt so e.g. a %t: prompt can never surface
/// a secret env.
pub fn search_envs(
    conn: &Connection,
    raw_query: &str,
    kind_filter: &[EnvKind],
    limit: i64,
) -> Result<Vec<StoredEnv>> {
    let kind_clause = if kind_filter.is_empty() {
        String::new()
    } else {
        let placeholders: Vec<String> = kind_filter
            .iter()
            .map(|k| format!("'{}'", k.as_str()))
            .collect();
        format!(" AND kind IN ({})", placeholders.join(","))
    };

    if raw_query.trim().is_empty() {
        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM envs WHERE 1=1{kind_clause} ORDER BY updated_at DESC LIMIT ?1"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![limit], map_row)?;
        return rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into);
    }

    let fts_query = build_fts_query(raw_query);
    let sql = format!(
        "SELECT {SELECT_COLUMNS_QUALIFIED}
         FROM envs_fts f
         JOIN envs e ON e.rowid = f.rowid
         WHERE envs_fts MATCH ?1{kind_clause}
         ORDER BY f.rank
         LIMIT ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![fts_query, limit], map_row)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    fn sample(id: &str, name: &str, kind: EnvKind) -> StoredEnv {
        StoredEnv {
            id: id.to_string(),
            name: name.to_string(),
            kind,
            value: b"value".to_vec(),
            nonce: None,
            description: format!("{name} description"),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn insert_and_find_by_name_roundtrip() {
        let conn = open_in_memory().unwrap();
        insert_env(&conn, &sample("id1", "apikey", EnvKind::Secret)).unwrap();
        let found = find_by_name(&conn, "apikey").unwrap().unwrap();
        assert_eq!(found.id, "id1");
        assert_eq!(found.kind, EnvKind::Secret);
    }

    #[test]
    fn name_uniqueness_is_enforced_by_schema() {
        let conn = open_in_memory().unwrap();
        insert_env(&conn, &sample("id1", "apikey", EnvKind::Secret)).unwrap();
        let err = insert_env(&conn, &sample("id2", "apikey", EnvKind::Text)).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("unique"));
    }

    #[test]
    fn list_all_returns_every_env() {
        let conn = open_in_memory().unwrap();
        insert_env(&conn, &sample("id1", "apikey", EnvKind::Secret)).unwrap();
        insert_env(&conn, &sample("id2", "username", EnvKind::Text)).unwrap();
        assert_eq!(list_all(&conn).unwrap().len(), 2);
    }

    #[test]
    fn delete_removes_env() {
        let conn = open_in_memory().unwrap();
        insert_env(&conn, &sample("id1", "apikey", EnvKind::Secret)).unwrap();
        assert!(delete_env(&conn, "id1").unwrap());
        assert!(find_by_name(&conn, "apikey").unwrap().is_none());
    }

    #[test]
    fn search_filters_by_compatible_kind() {
        let conn = open_in_memory().unwrap();
        insert_env(&conn, &sample("id1", "apikey", EnvKind::Secret)).unwrap();
        insert_env(&conn, &sample("id2", "username", EnvKind::Text)).unwrap();

        let secrets_only = search_envs(&conn, "", &[EnvKind::Secret], 10).unwrap();
        assert_eq!(secrets_only.len(), 1);
        assert_eq!(secrets_only[0].id, "id1");

        let all = search_envs(&conn, "", &[], 10).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn search_matches_name_and_description() {
        let conn = open_in_memory().unwrap();
        insert_env(&conn, &sample("id1", "apikey", EnvKind::Secret)).unwrap();
        let results = search_envs(&conn, "apikey", &[], 10).unwrap();
        assert_eq!(results.len(), 1);
    }
}
