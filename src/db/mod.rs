pub mod commands_repo;
pub mod env_repo;
pub mod history_repo;

use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS config (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    kdf_salt BLOB NOT NULL,
    canary_nonce BLOB NOT NULL,
    canary_cipher BLOB NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS commands (
    id TEXT PRIMARY KEY,
    template TEXT NOT NULL,
    description TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS command_variables (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    command_id TEXT NOT NULL REFERENCES commands(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    kind TEXT NOT NULL,
    name TEXT NOT NULL,
    UNIQUE(command_id, name)
);

CREATE VIRTUAL TABLE IF NOT EXISTS commands_fts USING fts5(description, template);

CREATE TRIGGER IF NOT EXISTS commands_ai AFTER INSERT ON commands BEGIN
    INSERT INTO commands_fts(rowid, description, template) VALUES (new.rowid, new.description, new.template);
END;

CREATE TRIGGER IF NOT EXISTS commands_ad AFTER DELETE ON commands BEGIN
    DELETE FROM commands_fts WHERE rowid = old.rowid;
END;

CREATE TRIGGER IF NOT EXISTS commands_au AFTER UPDATE ON commands BEGIN
    UPDATE commands_fts SET description = new.description, template = new.template WHERE rowid = new.rowid;
END;

CREATE TABLE IF NOT EXISTS envs (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL,
    value BLOB NOT NULL,
    nonce BLOB,
    description TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE VIRTUAL TABLE IF NOT EXISTS envs_fts USING fts5(name, description);

CREATE TRIGGER IF NOT EXISTS envs_ai AFTER INSERT ON envs BEGIN
    INSERT INTO envs_fts(rowid, name, description) VALUES (new.rowid, new.name, new.description);
END;

CREATE TRIGGER IF NOT EXISTS envs_ad AFTER DELETE ON envs BEGIN
    DELETE FROM envs_fts WHERE rowid = old.rowid;
END;

CREATE TRIGGER IF NOT EXISTS envs_au AFTER UPDATE ON envs BEGIN
    UPDATE envs_fts SET name = new.name, description = new.description WHERE rowid = new.rowid;
END;

CREATE TABLE IF NOT EXISTS history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    command_id TEXT NOT NULL,
    template TEXT NOT NULL,
    executed_at TEXT NOT NULL,
    bindings TEXT NOT NULL
);
"#;

pub fn open<P: AsRef<Path>>(path: P) -> Result<Connection> {
    let conn = Connection::open(path)?;
    init_schema(&conn)?;
    Ok(conn)
}

#[cfg(test)]
pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    init_schema(&conn)?;
    Ok(conn)
}

fn init_schema(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.execute_batch(SCHEMA)?;
    Ok(())
}

pub fn is_initialized(conn: &Connection) -> Result<bool> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM config WHERE id = 1", [], |row| row.get(0))?;
    Ok(count > 0)
}

pub struct Config {
    pub kdf_salt: Vec<u8>,
    pub canary_nonce: Vec<u8>,
    pub canary_cipher: Vec<u8>,
}

pub fn load_config(conn: &Connection) -> Result<Option<Config>> {
    let result = conn.query_row(
        "SELECT kdf_salt, canary_nonce, canary_cipher FROM config WHERE id = 1",
        [],
        |row| {
            Ok(Config {
                kdf_salt: row.get(0)?,
                canary_nonce: row.get(1)?,
                canary_cipher: row.get(2)?,
            })
        },
    );
    match result {
        Ok(cfg) => Ok(Some(cfg)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn save_config(conn: &Connection, cfg: &Config, created_at: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO config (id, kdf_salt, canary_nonce, canary_cipher, created_at) VALUES (1, ?1, ?2, ?3, ?4)",
        rusqlite::params![cfg.kdf_salt, cfg.canary_nonce, cfg.canary_cipher, created_at],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_initializes_cleanly() {
        let conn = open_in_memory().unwrap();
        assert!(!is_initialized(&conn).unwrap());
    }

    #[test]
    fn schema_is_idempotent() {
        let conn = open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        init_schema(&conn).unwrap();
    }

    #[test]
    fn save_and_load_config_roundtrip() {
        let conn = open_in_memory().unwrap();
        let cfg = Config {
            kdf_salt: vec![1, 2, 3],
            canary_nonce: vec![4, 5, 6],
            canary_cipher: vec![7, 8, 9],
        };
        save_config(&conn, &cfg, "2026-01-01T00:00:00Z").unwrap();
        assert!(is_initialized(&conn).unwrap());
        let loaded = load_config(&conn).unwrap().unwrap();
        assert_eq!(loaded.kdf_salt, vec![1, 2, 3]);
    }
}
