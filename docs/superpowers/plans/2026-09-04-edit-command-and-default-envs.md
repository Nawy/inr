# Edit Command + Default Envs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `inr e <id>` to interactively edit a saved command's description and template, and let each command variable carry an optional default env that `inr s` fills in automatically (with a graceful fallback if that env is later deleted), including across `inr export`/`inr import`.

**Architecture:** `command_variables` gets one new nullable `default_env_id TEXT` column (no FK — an id pointing nowhere is resolved lazily at read time, never cascaded). A new `StoredCommandVariable` type (kind+name+default_env_id) replaces the bare `CmdVariable` wherever variables are read back from the DB; `CmdVariable` itself stays as the lightweight "just parsed from a template" type. `inr a` and the new `inr e` share one interactive helper (`vars::review_default_envs`) to set/change/clear defaults per variable. `inr s`'s `vars::resolve_variable` auto-fills from a variable's default env with zero prompts when it still exists and is kind-compatible, falling back to today's manual prompt (with a one-line notice) otherwise. `inr export`/`inr import` carry each default by the env's *name* (not id, since ids are only meaningful within the machine that minted them) and re-link it against the destination's own envs after both commands and envs have been written for that import run.

**Tech Stack:** Rust (edition 2024), rusqlite (bundled SQLite), inquire 0.9.4 for prompts, serde/serde_json, anyhow. No new dependencies.

**Spec:** This plan's own header + the "Feature 1" / "Feature 2" summary agreed in conversation (grilled against `idea.md`); no separate spec file exists — the summary below is authoritative.

## Global Constraints

- Rust edition 2024, rustc 1.85+ — match the existing toolchain, no new crates.
- Follow existing patterns: single-letter CLI subcommands (`s`/`a`/`d`/`i`/`h`/`e`), `anyhow::Result` everywhere, `InquireError::OperationCanceled | OperationInterrupted` handled as a graceful `Ok(None)`/cancel rather than propagated.
- A secret's decrypted value must never be logged, printed, or written to SQL in plaintext — only ever held in `Zeroizing<...>` and set as a child-process env var (existing invariant in `shell.rs`/`vars.rs`, unchanged by this plan).
- Per the project's own testing convention (see README "Development" section): interactive terminal UI (raw `inquire` prompt/loop code) is not covered by automated tests and is verified by hand. Pure logic and DB-repo functions are always unit tested. This plan follows that split exactly — don't add tests for bare `inquire` prompt loops, do add them for everything else new.
- **Each task must leave the whole crate compiling and every test passing at its final step** — when a signature change and its only consumer are inseparable (as with `commands_repo::get_command_variables`'s return type and `vars::resolve_variable`'s parameter type), they belong in the same task, not split across a task boundary. Likewise, if a task adds a new non-defaulted struct field, every existing struct-literal construction of that type anywhere in the crate must be fixed within that same task.
- Migrations must be idempotent and safe on a database that already has data (`ALTER TABLE ... ADD COLUMN`, guarded by a `pragma_table_info` existence check) — never `DROP`/recreate a table with user data.

---

## Task 1: Schema migration — `default_env_id` column

**Files:**
- Modify: `src/db/mod.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `command_variables.default_env_id TEXT` column, present on every DB opened through `db::open`/`db::open_in_memory` (fresh or pre-existing). Later tasks (`commands_repo::set_command_variables`, `set_default_env`, `get_command_variables`) read/write this column directly via raw SQL, no Rust-level accessor needed from this task.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module at the bottom of `src/db/mod.rs`:

```rust
    #[test]
    fn migrate_schema_adds_default_env_id_to_pre_existing_db() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE commands (
                id TEXT PRIMARY KEY,
                template TEXT NOT NULL,
                description TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE command_variables (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                command_id TEXT NOT NULL,
                position INTEGER NOT NULL,
                kind TEXT NOT NULL,
                name TEXT NOT NULL
            );",
        )
        .unwrap();

        migrate_schema(&conn).unwrap();

        let has_column: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('command_variables') WHERE name = 'default_env_id'")
            .unwrap()
            .exists([])
            .unwrap();
        assert!(has_column);
    }

    #[test]
    fn migrate_schema_is_idempotent_when_column_already_present() {
        let conn = open_in_memory().unwrap();
        migrate_schema(&conn).unwrap();
        migrate_schema(&conn).unwrap();
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test db::tests::migrate_schema -- --nocapture`
Expected: FAIL with `cannot find function 'migrate_schema' in this scope`

- [ ] **Step 3: Implement the migration**

In `src/db/mod.rs`, add `default_env_id TEXT` to the `command_variables` table in the `SCHEMA` constant (so brand-new databases get it for free):

```rust
CREATE TABLE IF NOT EXISTS command_variables (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    command_id TEXT NOT NULL REFERENCES commands(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    kind TEXT NOT NULL,
    name TEXT NOT NULL,
    default_env_id TEXT,
    UNIQUE(command_id, name)
);
```

Then add the migration function and call it from `init_schema`:

```rust
/// Adds columns introduced after a user's database was first created.
/// `CREATE TABLE IF NOT EXISTS` (in `SCHEMA`) is a no-op on a table that
/// already exists, so a pre-existing `command_variables` table never picks
/// up new columns on its own - this patches it in, once, idempotently.
/// Existing rows get NULL, which the rest of the code already treats as
/// "no default env set".
fn migrate_schema(conn: &Connection) -> Result<()> {
    let has_default_env_id: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('command_variables') WHERE name = 'default_env_id'")?
        .exists([])?;
    if !has_default_env_id {
        conn.execute("ALTER TABLE command_variables ADD COLUMN default_env_id TEXT", [])?;
    }
    Ok(())
}

fn init_schema(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.execute_batch(SCHEMA)?;
    migrate_schema(conn)?;
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test db::`
Expected: PASS (including the two new tests and the pre-existing `schema_initializes_cleanly` / `schema_is_idempotent` / `save_and_load_config_roundtrip`)

- [ ] **Step 5: Commit**

```bash
git add src/db/mod.rs
git commit -m "$(cat <<'EOF'
Add default_env_id column to command_variables with idempotent migration

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015erh8QLJbinntztAzonBBn
EOF
)"
```

---

## Task 2: `StoredCommandVariable` and variable diffing

**Files:**
- Modify: `src/models.rs`

**Interfaces:**
- Consumes: `CmdVariable`, `CmdVarKind` (existing, in the same file).
- Produces:
  - `pub struct StoredCommandVariable { pub var: CmdVariable, pub default_env_id: Option<String> }` (`Debug, Clone, PartialEq, Eq`)
  - `pub struct VariableDiff { pub kept: Vec<CmdVariable>, pub added: Vec<CmdVariable>, pub removed: Vec<StoredCommandVariable> }` with `pub fn is_unchanged(&self) -> bool`
  - `pub fn diff_variables(old: &[StoredCommandVariable], new: &[CmdVariable]) -> VariableDiff`

Both types are consumed by Task 3 (`commands_repo` + `vars.rs`) and Task 5 (`commands/edit.rs`).

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module at the bottom of `src/models.rs`:

```rust
    fn cmdvar(kind: CmdVarKind, name: &str) -> CmdVariable {
        CmdVariable { kind, name: name.to_string() }
    }

    fn stored(kind: CmdVarKind, name: &str, default_env_id: Option<&str>) -> StoredCommandVariable {
        StoredCommandVariable {
            var: cmdvar(kind, name),
            default_env_id: default_env_id.map(str::to_string),
        }
    }

    #[test]
    fn diff_variables_classifies_kept_added_removed() {
        let old = vec![
            stored(CmdVarKind::Text, "host", Some("env1")),
            stored(CmdVarKind::Secret, "token", None),
        ];
        let new = vec![
            cmdvar(CmdVarKind::Text, "host"),  // kept (same name+kind)
            cmdvar(CmdVarKind::Number, "port"), // added
            // "token" is gone -> removed
        ];

        let diff = diff_variables(&old, &new);
        assert_eq!(diff.kept, vec![cmdvar(CmdVarKind::Text, "host")]);
        assert_eq!(diff.added, vec![cmdvar(CmdVarKind::Number, "port")]);
        assert_eq!(diff.removed, vec![stored(CmdVarKind::Secret, "token", None)]);
        assert!(!diff.is_unchanged());
    }

    #[test]
    fn diff_variables_treats_kind_change_as_remove_plus_add() {
        let old = vec![stored(CmdVarKind::Text, "count", Some("env1"))];
        let new = vec![cmdvar(CmdVarKind::Number, "count")];

        let diff = diff_variables(&old, &new);
        assert!(diff.kept.is_empty());
        assert_eq!(diff.added, vec![cmdvar(CmdVarKind::Number, "count")]);
        assert_eq!(diff.removed, vec![stored(CmdVarKind::Text, "count", Some("env1"))]);
    }

    #[test]
    fn diff_variables_identical_sets_is_unchanged() {
        let old = vec![stored(CmdVarKind::Text, "host", None)];
        let new = vec![cmdvar(CmdVarKind::Text, "host")];

        let diff = diff_variables(&old, &new);
        assert!(diff.is_unchanged());
        assert_eq!(diff.kept, vec![cmdvar(CmdVarKind::Text, "host")]);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test models::tests::diff_variables -- --nocapture`
Expected: FAIL with `cannot find type 'StoredCommandVariable'` / `cannot find function 'diff_variables'`

- [ ] **Step 3: Implement**

Add below the existing `StoredEnv` struct in `src/models.rs`:

```rust
/// A command variable as actually stored in the DB: name/kind plus its
/// optional default env. `default_env_id` is an unconstrained id (no FK) -
/// the env it points to may have been deleted since; that's resolved
/// lazily by whoever reads it (`vars::resolve_variable`), never tracked or
/// cascaded here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCommandVariable {
    pub var: CmdVariable,
    pub default_env_id: Option<String>,
}

/// Result of comparing a command's previously-stored variables against a
/// freshly re-parsed set from an edited template, matched by (name, kind).
/// Used by `inr e` to summarize what will change before saving - a
/// variable whose name is reused with a different kind counts as one
/// removal plus one addition, never a "kept".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariableDiff {
    pub kept: Vec<CmdVariable>,
    pub added: Vec<CmdVariable>,
    pub removed: Vec<StoredCommandVariable>,
}

impl VariableDiff {
    pub fn is_unchanged(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

pub fn diff_variables(old: &[StoredCommandVariable], new: &[CmdVariable]) -> VariableDiff {
    let matches = |o: &StoredCommandVariable, n: &CmdVariable| o.var.name == n.name && o.var.kind == n.kind;

    let kept = new.iter().filter(|n| old.iter().any(|o| matches(o, n))).cloned().collect();
    let added = new.iter().filter(|n| !old.iter().any(|o| matches(o, n))).cloned().collect();
    let removed = old.iter().filter(|o| !new.iter().any(|n| matches(o, n))).cloned().collect();

    VariableDiff { kept, added, removed }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test models::`
Expected: PASS (all `models` tests, old and new)

- [ ] **Step 5: Commit**

```bash
git add src/models.rs
git commit -m "$(cat <<'EOF'
Add StoredCommandVariable and diff_variables for template-edit diffing

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015erh8QLJbinntztAzonBBn
EOF
)"
```

---

## Task 3: `commands_repo` + `vars.rs` — default-env-aware storage and auto-resolve

This is one task, not two, because the two halves are inseparable: changing what `commands_repo::get_command_variables` returns is meaningless (and won't compile) until its one caller, `vars::resolve_variable`, is updated to match — see Global Constraints. Both land together so the crate compiles and every test passes at the end of this task.

**Files:**
- Modify: `src/db/commands_repo.rs`
- Modify: `src/vars.rs`
- Modify: `src/commands/import.rs` (one existing test only — return type change)

**Interfaces:**
- Consumes: `StoredCommandVariable` (Task 2), existing `CmdVariable`, `CmdVarKind`, `StoredCommand`, `KeyCache`, `crypto`.
- Produces:
  - `commands_repo::get_command_variables(conn, command_id: &str) -> Result<Vec<StoredCommandVariable>>` (return type changed from `Vec<CmdVariable>`)
  - `commands_repo::set_command_variables(conn: &Connection, command_id: &str, new_vars: &[CmdVariable], previous: &[StoredCommandVariable]) -> Result<()>` — replaces all rows for `command_id`, carrying `default_env_id` forward for any (name, kind) match found in `previous`.
  - `commands_repo::set_default_env(conn: &Connection, command_id: &str, var_name: &str, default_env_id: Option<&str>) -> Result<()>`
  - `commands_repo::update_command(conn: &Connection, id: &str, template: &str, description: &str, updated_at: &str) -> Result<()>`
  - `commands_repo::get_variable_default_names(conn: &Connection, command_id: &str) -> Result<std::collections::BTreeMap<String, String>>` (variable name -> default env's *name*, only for defaults whose env still exists) — used by Task 6 (`export.rs`).
  - `vars::resolve_variable(conn: &Rc<Connection>, keys: &mut KeyCache, var: &StoredCommandVariable) -> Result<Option<ResolvedVar>>` (signature changed: takes `&StoredCommandVariable` instead of `&CmdVariable`; auto-fills from a still-existing default env with zero prompts, falling back to the manual prompt with a notice otherwise)
  - `vars::KeyCache::preloaded` (test-only constructor, used by this task's own tests)

These are consumed by: Task 4 (`add.rs`, via a different `vars.rs` addition), Task 5 (`commands/edit.rs`), Task 6 (`export.rs`), Task 7 (`import.rs`). `src/commands/search.rs` needs **no code change** — `cmd_vars`'s type and the `var` it hands to `resolve_variable` both follow the new signatures automatically; it's listed here only as "verify it still compiles", not "modify".

- [ ] **Step 1: Write the failing tests**

Replace the existing `insert_and_get_roundtrip_with_variables` test in `src/db/commands_repo.rs` (its assertions no longer compile once `get_command_variables`'s return type changes) and add new tests. Replace this block:

```rust
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
```

with:

```rust
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
```

In `src/vars.rs`, add a `#[cfg(test)] mod tests` block at the bottom of the file (there isn't one yet):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{self, env_repo};
    use crate::models::{CmdVarKind, EnvKind, StoredEnv};
    use crate::shell::BindingSource;

    fn text_env(id: &str, name: &str, value: &str) -> StoredEnv {
        StoredEnv {
            id: id.to_string(),
            name: name.to_string(),
            kind: EnvKind::Text,
            value: value.as_bytes().to_vec(),
            nonce: None,
            description: String::new(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn resolve_variable_uses_default_env_without_prompting_for_text_kind() {
        let conn = db::open_in_memory().unwrap();
        env_repo::insert_env(&conn, &text_env("env1", "username", "alice")).unwrap();
        let conn = Rc::new(conn);
        let mut keys = KeyCache::new(conn.as_ref());

        let var = StoredCommandVariable {
            var: CmdVariable { kind: CmdVarKind::Text, name: "user".to_string() },
            default_env_id: Some("env1".to_string()),
        };

        let resolved = resolve_variable(&conn, &mut keys, &var).unwrap().unwrap();
        assert_eq!(resolved.value.as_str(), "alice");
        assert_eq!(resolved.source, BindingSource::Env("username".to_string()));
    }

    #[test]
    fn resolve_variable_uses_default_env_for_secret_and_decrypts_with_preloaded_key() {
        let conn = db::open_in_memory().unwrap();
        let salt = crypto::random_salt();
        let key = crypto::derive_key("hunter2", &salt).unwrap();
        let (nonce, cipher) = crypto::encrypt(&key, b"sk-secret").unwrap();
        let env = StoredEnv {
            id: "env1".to_string(),
            name: "apikey".to_string(),
            kind: EnvKind::Secret,
            value: cipher,
            nonce: Some(nonce.to_vec()),
            description: String::new(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        };
        env_repo::insert_env(&conn, &env).unwrap();
        let conn = Rc::new(conn);
        let mut keys = KeyCache::preloaded(conn.as_ref(), key);

        let var = StoredCommandVariable {
            var: CmdVariable { kind: CmdVarKind::Secret, name: "token".to_string() },
            default_env_id: Some("env1".to_string()),
        };

        let resolved = resolve_variable(&conn, &mut keys, &var).unwrap().unwrap();
        assert_eq!(resolved.value.as_str(), "sk-secret");
        assert_eq!(resolved.source, BindingSource::Env("apikey".to_string()));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test db::commands_repo::` and `cargo test vars::`
Expected: FAIL to compile (`set_default_env`, `set_command_variables`, `update_command`, `get_variable_default_names` don't exist yet; `get_command_variables` still returns `Vec<CmdVariable>`; `resolve_variable` still takes `&CmdVariable`; `KeyCache::preloaded` doesn't exist)

- [ ] **Step 3: Implement `commands_repo`**

Update the `use` block at the top of `src/db/commands_repo.rs`:

```rust
use crate::db::env_repo;
use crate::models::{CmdVarKind, CmdVariable, StoredCommand, StoredCommandVariable};
use anyhow::Result;
use rusqlite::{params, Connection, Row};
use std::collections::BTreeMap;
```

Replace the body of `insert_command_raw` from the `for (i, v) in vars.iter().enumerate() { ... }` loop onward:

```rust
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
```

Replace `get_command_variables` entirely:

```rust
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
        if let Some(env_id) = v.default_env_id {
            if let Some(env) = env_repo::get_by_id(conn, &env_id)? {
                out.insert(v.var.name, env.name);
            }
        }
    }
    Ok(out)
}
```

- [ ] **Step 4: Implement `vars.rs`**

Update the `use` block at the top of `src/vars.rs`:

```rust
use crate::crypto::{self, DerivedKey};
use crate::db::{self, commands_repo, env_repo};
use crate::interactive;
use crate::models::{CmdVarKind, CmdVariable, EnvKind, StoredCommandVariable};
use crate::shell::{BindingSource, ResolvedVar};
use crate::tui::{self, EnvLookupAutocomplete};
use anyhow::{anyhow, Result};
use inquire::{InquireError, Password, PasswordDisplayMode, Select, Text};
use rusqlite::Connection;
use std::rc::Rc;
use zeroize::Zeroizing;
```

(this adds `commands_repo` to the existing `db::{self, env_repo}` import and `StoredCommandVariable` to the `models` import — `Select` was already imported for `resolve_secret_variable`)

Add a test-only constructor to `KeyCache`, right after `new`:

```rust
impl<'a> KeyCache<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn, key: None }
    }

    /// Test-only: builds a `KeyCache` with the key already unlocked, so
    /// tests can exercise the secret-decryption path without going through
    /// an interactive password prompt.
    #[cfg(test)]
    pub(crate) fn preloaded(conn: &'a Connection, key: DerivedKey) -> Self {
        Self { conn, key: Some(key) }
    }

    pub fn get(&mut self) -> Result<&DerivedKey> {
        // ... unchanged
    }
}
```

Replace the existing `resolve_variable` function:

```rust
/// Prompts for one command variable's value at run time. Returns `Ok(None)`
/// if the user cancelled (Esc). If `var` has a default env set and that env
/// still exists, it's used with zero prompts (secret defaults still go
/// through `KeyCache`, same as an explicit `@`-lookup) - otherwise this
/// falls back to the normal manual prompt, after printing a one-line notice
/// if the default was set but its env has since been deleted.
pub fn resolve_variable(
    conn: &Rc<Connection>,
    keys: &mut KeyCache,
    var: &StoredCommandVariable,
) -> Result<Option<ResolvedVar>> {
    if let Some(env_id) = &var.default_env_id {
        match env_repo::get_by_id(conn, env_id)? {
            Some(env) => return resolve_from_default_env(keys, &var.var, env).map(Some),
            None => println!(
                "Default env for '{}' no longer exists - enter a value:",
                var.var.name
            ),
        }
    }
    match var.var.kind {
        CmdVarKind::Secret => resolve_secret_variable(conn, keys, &var.var),
        CmdVarKind::Text | CmdVarKind::Number => resolve_literal_or_env_variable(conn, &var.var),
    }
}

/// Resolves a variable straight from its default env - no prompt at all.
/// Defense in depth: the kind compatibility was already guaranteed at the
/// time the default was set (`vars::prompt_default_env_choice`, added in
/// the next task, only offers compatible envs), this just double-checks it
/// never silently drifted.
fn resolve_from_default_env(
    keys: &mut KeyCache,
    var: &CmdVariable,
    env: crate::models::StoredEnv,
) -> Result<ResolvedVar> {
    debug_assert!(env.kind.compatible_with(var.kind));
    let value = match env.kind {
        EnvKind::Secret => decrypt_env_value(keys, &env)?,
        _ => Zeroizing::new(String::from_utf8_lossy(&env.value).to_string()),
    };
    Ok(ResolvedVar {
        var: var.clone(),
        value,
        source: BindingSource::Env(env.name.clone()),
    })
}
```

Every other function in `vars.rs` (`compatible_env_kinds`, `decrypt_env_value`, `resolve_literal_or_env_variable`, `resolve_secret_variable`) is unchanged — they already take `&CmdVariable`, which `var.var` (inside `StoredCommandVariable`) still supplies.

In `src/commands/import.rs`, the test `insert_exported_command_derives_variables_from_template` asserts `vars[0].name` / `vars[1].name` directly - update to go through `.var`:

```rust
    #[test]
    fn insert_exported_command_derives_variables_from_template() {
        let conn = open_in_memory().unwrap();
        insert_exported_command(&conn, exported_command("ssh %s:host -p %n:port", "connect")).unwrap();

        let vars = commands_repo::get_command_variables(&conn, "cmd1").unwrap();
        assert_eq!(vars.len(), 2);
        assert_eq!(vars[0].var.name, "host");
        assert_eq!(vars[1].var.name, "port");
    }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo build && cargo test`
Expected: PASS — the whole crate compiles and every test (old and new) passes.

- [ ] **Step 6: Commit**

```bash
git add src/db/commands_repo.rs src/vars.rs src/commands/import.rs
git commit -m "$(cat <<'EOF'
Store default_env_id per variable and auto-resolve it at run time

commands_repo::get_command_variables now returns StoredCommandVariable
(name/kind plus an optional, unconstrained default_env_id); adds
set_command_variables (replace-with-carry-forward), set_default_env,
update_command, and get_variable_default_names. vars::resolve_variable
fills a variable straight from its default env - decrypting via KeyCache
for a secret default - with zero prompts when that env still exists and
is kind-compatible, falling back to the manual prompt (with a notice)
otherwise.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015erh8QLJbinntztAzonBBn
EOF
)"
```

---

## Task 4: `vars.rs` — interactive default-env review, wired into `inr a`

**Files:**
- Modify: `src/vars.rs`
- Modify: `src/commands/add.rs`

**Interfaces:**
- Consumes: `commands_repo::set_default_env` (Task 3), `tui::select_env` (existing), `compatible_env_kinds` (existing private fn in `vars.rs`).
- Produces:
  - `pub enum DefaultEnvChoice { Unchanged, Cleared, Set(StoredEnv) }`
  - `pub fn prompt_default_env_choice(conn: &Rc<Connection>, var: &CmdVariable, current: Option<&StoredEnv>) -> Result<DefaultEnvChoice>`
  - `pub fn review_default_envs(conn: &Rc<Connection>, command_id: &str, vars: &[CmdVariable], current_defaults: &HashMap<String, StoredEnv>) -> Result<()>`

`review_default_envs` is consumed by `add.rs` (this task) and `commands/edit.rs` (Task 5). Per Global Constraints, `prompt_default_env_choice`/`review_default_envs` are raw interactive `inquire` loops — no automated tests, verified by hand (matches how `resolve_secret_variable`/`resolve_literal_or_env_variable` are already left untested).

- [ ] **Step 1: No new automated test for this task**

This task is pure interactive UI (an `inquire::Select` plus a call into the already-tested `tui::select_env`/`commands_repo::set_default_env`), which the project's own convention (README "Development" section, restated in Global Constraints) explicitly leaves to manual verification. Skip straight to implementation; manual verification happens at the end of Task 5, once `inr a`'s full flow can be exercised end-to-end (`cargo run -- a "..."`).

- [ ] **Step 2: Implement**

In `src/vars.rs`, update the `models` import to add `StoredEnv`, and add `HashMap` to the top-level imports:

```rust
use crate::models::{CmdVarKind, CmdVariable, EnvKind, StoredCommandVariable, StoredEnv};
use std::collections::HashMap;
```

Add near the bottom of `src/vars.rs` (after `resolve_secret_variable`):

```rust
/// What the user chose when asked about one variable's default env.
pub enum DefaultEnvChoice {
    /// Nothing selected that changes the current state (cancelled, or
    /// explicitly kept as-is).
    Unchanged,
    /// Explicitly asked to remove the default.
    Cleared,
    /// Picked a (kind-compatible) env to use as the default from now on.
    Set(StoredEnv),
}

/// Asks whether/how to set variable `var`'s default env, given its
/// `current` value (if any and if it still resolves to a real env).
/// Returns `Unchanged` on Esc/cancel at any point.
pub fn prompt_default_env_choice(
    conn: &Rc<Connection>,
    var: &CmdVariable,
    current: Option<&StoredEnv>,
) -> Result<DefaultEnvChoice> {
    let (message, options): (String, Vec<&str>) = match current {
        Some(env) => (
            format!("{} ({}) - default env is @{}:", var.name, var.kind, env.name),
            vec!["Keep current default", "Change default env", "Remove default"],
        ),
        None => (
            format!("{} ({}) - set a default env?", var.name, var.kind),
            vec!["No default", "Set a default env"],
        ),
    };

    let choice = Select::new(&message, options).prompt();
    let choice = match choice {
        Ok(c) => c,
        Err(InquireError::OperationCanceled) | Err(InquireError::OperationInterrupted) => {
            return Ok(DefaultEnvChoice::Unchanged)
        }
        Err(e) => return Err(e.into()),
    };

    match choice {
        "Keep current default" | "No default" => Ok(DefaultEnvChoice::Unchanged),
        "Remove default" => Ok(DefaultEnvChoice::Cleared),
        _ => {
            let kind_filter = compatible_env_kinds(var.kind);
            match tui::select_env(conn, &kind_filter, "Search envs:")? {
                Some(env) => Ok(DefaultEnvChoice::Set(env)),
                None => Ok(DefaultEnvChoice::Unchanged),
            }
        }
    }
}

/// Walks each of a command's variables, offering to set/change/clear its
/// default env, and persists whatever was chosen. `current_defaults` maps
/// variable name -> its currently-set (and still-existing) default env;
/// pass an empty map for a brand new command.
pub fn review_default_envs(
    conn: &Rc<Connection>,
    command_id: &str,
    vars: &[CmdVariable],
    current_defaults: &HashMap<String, StoredEnv>,
) -> Result<()> {
    for var in vars {
        let current = current_defaults.get(&var.name);
        match prompt_default_env_choice(conn, var, current)? {
            DefaultEnvChoice::Unchanged => {}
            DefaultEnvChoice::Cleared => {
                commands_repo::set_default_env(conn, command_id, &var.name, None)?;
                println!("Default env cleared for '{}'.", var.name);
            }
            DefaultEnvChoice::Set(env) => {
                commands_repo::set_default_env(conn, command_id, &var.name, Some(&env.id))?;
                println!("Default env for '{}' set to @{}.", var.name, env.name);
            }
        }
    }
    Ok(())
}
```

Now wire it into `src/commands/add.rs`. Add `use std::collections::HashMap;`, `use std::rc::Rc;`, and `use crate::vars;` to its imports. Change the end of `run`:

```rust
    commands_repo::insert_command(&mut conn, &cmd, &vars)?;
    println!("Saved. id: {id}");

    if !vars.is_empty() {
        let conn = Rc::new(conn);
        vars::review_default_envs(&conn, &id, &vars, &HashMap::new())?;
    }
    Ok(())
```

(replacing the old tail: `commands_repo::insert_command(&mut conn, &cmd, &vars)?;\n\n    println!("Saved. id: {id}");\n    Ok(())`)

- [ ] **Step 3: Run the full test suite to confirm nothing broke**

Run: `cargo build && cargo test`
Expected: PASS

- [ ] **Step 4: Manual smoke test**

Run: `cargo run -- a "echo %t:msg"` against a scratch DB (point `APPDATA`/`XDG_DATA_HOME` at a temp dir first if you don't want to touch your real vault - see Task 5's manual-verification step for the same setup) and confirm you're offered "set a default env?" for `msg` after saving, and that declining still leaves the command usable.

- [ ] **Step 5: Commit**

```bash
git add src/vars.rs src/commands/add.rs
git commit -m "$(cat <<'EOF'
Offer to set a default env per variable right after inr a saves a command

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015erh8QLJbinntztAzonBBn
EOF
)"
```

---

## Task 5: `inr e <id>` — interactive command edit

**Files:**
- Create: `src/commands/edit.rs`
- Modify: `src/commands/mod.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `commands_repo::{get_command, get_command_variables, update_command, set_command_variables}` (Task 3), `models::{diff_variables, VariableDiff, StoredCommandVariable, CmdVariable, StoredEnv}` (Task 2 / existing), `vars::review_default_envs` (Task 4), `placeholder::parse_placeholders` (existing), `interactive::confirm` (existing).
- Produces: `pub fn run(id: String) -> Result<()>` in `commands::edit`, wired to `inr e <id>`.

- [ ] **Step 1: No new repo-level test for this task**

`edit.rs`'s `run` is entirely interactive orchestration (prompts + calls into already-tested `commands_repo`/`vars` functions), so per Global Constraints it isn't unit tested directly — same treatment as `commands/add.rs::run` and `commands/delete.rs::run` today (neither has tests). The logic worth testing in isolation (`diff_variables`) was already covered in Task 2. Skip straight to implementation; verify manually in Step 3.

- [ ] **Step 2: Implement**

Create `src/commands/edit.rs`:

```rust
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
    let conn = db::open(paths::db_path()?)?;

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
    commands_repo::update_command(&conn, &id, &new_template, &new_description, &now)?;
    commands_repo::set_command_variables(&conn, &id, &new_vars, &old_vars)?;
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
        if let Some(env_id) = v.default_env_id {
            if let Some(env) = env_repo::get_by_id(conn, &env_id)? {
                out.insert(v.var.name, env);
            }
        }
    }
    Ok(out)
}
```

In `src/commands/mod.rs`, add the new module (alphabetical, matching the existing list):

```rust
pub mod add;
pub mod delete;
pub mod edit;
pub mod env_add;
pub mod env_remove;
pub mod env_search;
pub mod export;
pub mod history;
pub mod import;
pub mod init;
pub mod search;
```

In `src/main.rs`, add the `E` variant to the `Commands` enum (after `D`, before `I`, so `d`→delete / `e`→edit read naturally together):

```rust
    /// Delete a command by id
    D {
        /// The command's nanoid
        id: String,
    },
    /// Edit a command's description and text, interactively
    E {
        /// The command's nanoid
        id: String,
    },
    /// Initialize inr (set the master password)
    I,
```

And in `main()`'s `match cli.command`:

```rust
        Commands::D { id } => commands::delete::run(id),
        Commands::E { id } => commands::edit::run(id),
        Commands::I => commands::init::run(),
```

- [ ] **Step 3: Manual verification**

Run: `cargo build` — expect a clean build.

Then, against a scratch vault (e.g. on Windows, `$env:APPDATA = "$env:TEMP\inr-scratch"` in a fresh shell before running `cargo run --`, so you don't touch your real `%APPDATA%\inr`), exercise:
1. `cargo run -- i` (set a master password)
2. `cargo run -- a "curl %t:host"` (create a command, decline the default-env offer)
3. `cargo run -- e <id>` — confirm the description/template prompts show the current text pre-filled and editable, changing only the template's variable (e.g. to `curl %s:host`) shows the "+ host (secret)" / "- host (text)" diff and asks to confirm, and that declining leaves the command untouched (`inr s` still shows the old template).
4. Re-run `inr e <id>`, accept the change this time, and confirm `inr s` now prompts for `host` as a secret.
5. `cargo run -- e does-not-exist` prints `No command with id 'does-not-exist'.` and exits cleanly.

- [ ] **Step 4: Commit**

```bash
git add src/commands/edit.rs src/commands/mod.rs src/main.rs
git commit -m "$(cat <<'EOF'
Add inr e <id> to interactively edit a command's description and text

Editing the template re-parses its placeholders and diffs them against
the stored ones (matched by name+kind); changes are summarized and
confirmed before saving, and each surviving/new variable can have its
default env reviewed afterward via vars::review_default_envs.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015erh8QLJbinntztAzonBBn
EOF
)"
```

---

## Task 6: `inr export` — carry default envs by name

**Files:**
- Modify: `src/transfer.rs`
- Modify: `src/commands/export.rs`
- Modify: `src/commands/import.rs` (one-line test-helper fix, see below)

**Interfaces:**
- Consumes: `commands_repo::get_variable_default_names` (Task 3).
- Produces: `ExportedCommand.variable_defaults: BTreeMap<String, String>` (variable name -> default env's name), consumed by Task 7 (`import.rs`).

`ExportedCommand` gaining a new, non-`Default`-derived field breaks every existing struct-literal construction of it, crate-wide, until each is fixed — per Global Constraints, that fix belongs in this task, not deferred. The only such construction outside this task's own files is the `exported_command` test helper in `src/commands/import.rs`.

- [ ] **Step 1: Write the failing tests**

In `src/transfer.rs`, update `sample_payload()` (used by all tests in that module) to populate the new field, and add two new tests. Replace the `ExportedCommand` literal inside `sample_payload()`:

```rust
    fn sample_payload() -> TransferPayload {
        TransferPayload {
            exported_at: "2026-01-01T00:00:00Z".to_string(),
            commands: vec![ExportedCommand {
                id: "cmd1".to_string(),
                template: "ssh %s:host".to_string(),
                description: "connect".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
                variable_defaults: BTreeMap::from([("host".to_string(), "prod-host".to_string())]),
            }],
            envs: vec![ExportedEnv {
                id: "env1".to_string(),
                name: "apikey".to_string(),
                kind: "secret".to_string(),
                value: "sk-super-secret".to_string(),
                description: "api key".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
            }],
        }
    }
```

Add two new tests alongside `encode_decode_roundtrip`:

```rust
    #[test]
    fn encode_decode_roundtrips_variable_defaults() {
        let payload = sample_payload();
        let bytes = encode(&payload, "correct horse battery staple").unwrap();
        let decoded = decode(&bytes, "correct horse battery staple").unwrap();
        assert_eq!(
            decoded.commands[0].variable_defaults.get("host"),
            Some(&"prod-host".to_string())
        );
    }

    #[test]
    fn decode_defaults_variable_defaults_to_empty_when_field_is_absent() {
        // Simulates a transfer file written before this field existed:
        // the same payload shape, minus `variable_defaults` in the JSON.
        #[derive(serde::Serialize)]
        struct OldExportedCommand {
            id: String,
            template: String,
            description: String,
            created_at: String,
            updated_at: String,
        }
        #[derive(serde::Serialize)]
        struct OldPayload {
            exported_at: String,
            commands: Vec<OldExportedCommand>,
            envs: Vec<ExportedEnv>,
        }
        let old = OldPayload {
            exported_at: "2026-01-01T00:00:00Z".to_string(),
            commands: vec![OldExportedCommand {
                id: "cmd1".to_string(),
                template: "echo hi".to_string(),
                description: "greet".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
            }],
            envs: vec![],
        };
        let json = serde_json::to_vec(&old).unwrap();
        let salt = crypto::random_salt();
        let key = crypto::derive_key("pw", &salt).unwrap();
        let (nonce, ciphertext) = crypto::encrypt(&key, &json).unwrap();

        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.push(FORMAT_VERSION);
        bytes.extend_from_slice(&salt);
        bytes.extend_from_slice(&nonce);
        bytes.extend_from_slice(&ciphertext);

        let decoded = decode(&bytes, "pw").unwrap();
        assert!(decoded.commands[0].variable_defaults.is_empty());
    }
```

Add `use std::collections::BTreeMap;` to the top of `src/transfer.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test transfer::`
Expected: FAIL to compile (`ExportedCommand` has no field `variable_defaults`)

- [ ] **Step 3: Implement**

In `src/transfer.rs`, update the `ExportedCommand` struct:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedCommand {
    pub id: String,
    pub template: String,
    pub description: String,
    pub created_at: String,
    pub updated_at: String,
    /// Variable name -> default env's *name* (not id - ids are only
    /// meaningful within the machine that generated them; names are the
    /// natural key envs are matched by everywhere else). Sparse: only
    /// variables with a default appear. `serde(default)` so a transfer
    /// file written before this field existed still decodes cleanly.
    #[serde(default)]
    pub variable_defaults: BTreeMap<String, String>,
}
```

In `src/commands/export.rs`, populate the new field when building `exported_commands` - replace the existing `let exported_commands: Vec<ExportedCommand> = commands.iter().map(|c| ExportedCommand { ... }).collect();` block with:

```rust
    let mut exported_commands = Vec::with_capacity(commands.len());
    for c in &commands {
        exported_commands.push(ExportedCommand {
            id: c.id.clone(),
            template: c.template.clone(),
            description: c.description.clone(),
            created_at: c.created_at.clone(),
            updated_at: c.updated_at.clone(),
            variable_defaults: commands_repo::get_variable_default_names(&conn, &c.id)?,
        });
    }
```

(a plain `for` loop, since `get_variable_default_names` returns a `Result` and needs `?`, which doesn't fit cleanly inside a `.map()` closure)

Finally, fix the one existing construction of `ExportedCommand` outside this task's files: in `src/commands/import.rs`, the test helper `exported_command` needs the new field. Update it:

```rust
    fn exported_command(template: &str, description: &str) -> ExportedCommand {
        ExportedCommand {
            id: "cmd1".to_string(),
            template: template.to_string(),
            description: description.to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            variable_defaults: std::collections::BTreeMap::new(),
        }
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo build && cargo test`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/transfer.rs src/commands/export.rs src/commands/import.rs
git commit -m "$(cat <<'EOF'
Carry each variable's default env (by name) in inr export's transfer file

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015erh8QLJbinntztAzonBBn
EOF
)"
```

---

## Task 7: `inr import` — re-link default envs on the destination

**Files:**
- Modify: `src/commands/import.rs`

**Interfaces:**
- Consumes: `ExportedCommand.variable_defaults` (Task 6), `env_repo::find_by_name`, `commands_repo::{get_command_variables, set_default_env}` (Task 3), `EnvKind::compatible_with` (existing).
- Produces: `fn apply_variable_defaults(conn: &Connection, pending: &[(String, BTreeMap<String, String>)]) -> Result<(usize, usize)>` (linked, skipped counts) - a pure, DB-using-but-non-interactive function, unit tested directly (same pattern as `diff_commands`/`diff_envs`/`build_stored_env` already in this file). Wired into `run()`.

- [ ] **Step 1: Write the failing tests**

Add a second test helper alongside the existing `exported_command` (from Task 6) in `src/commands/import.rs`'s `tests` module:

```rust
    fn exported_command_with_defaults(
        template: &str,
        description: &str,
        variable_defaults: &[(&str, &str)],
    ) -> ExportedCommand {
        ExportedCommand {
            variable_defaults: variable_defaults
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..exported_command(template, description)
        }
    }

    fn test_key() -> DerivedKey {
        let salt = crypto::random_salt();
        crypto::derive_key("test-password", &salt).unwrap()
    }

    #[test]
    fn apply_variable_defaults_links_when_compatible_env_exists_locally() {
        let conn = open_in_memory().unwrap();
        insert_exported_command(
            &conn,
            exported_command_with_defaults("curl %t:region", "curl", &[("region", "region")]),
        )
        .unwrap();
        let env = build_stored_env(exported_env("region", "text", "us-east"), None).unwrap();
        env_repo::insert_env(&conn, &env).unwrap();

        let pending = vec![(
            "cmd1".to_string(),
            std::collections::BTreeMap::from([("region".to_string(), "region".to_string())]),
        )];
        let (linked, skipped) = apply_variable_defaults(&conn, &pending).unwrap();
        assert_eq!(linked, 1);
        assert_eq!(skipped, 0);

        let vars = commands_repo::get_command_variables(&conn, "cmd1").unwrap();
        assert_eq!(vars[0].default_env_id, Some(env.id));
    }

    #[test]
    fn apply_variable_defaults_skips_when_no_matching_env_locally() {
        let conn = open_in_memory().unwrap();
        insert_exported_command(
            &conn,
            exported_command_with_defaults("curl %t:region", "curl", &[("region", "region")]),
        )
        .unwrap();

        let pending = vec![(
            "cmd1".to_string(),
            std::collections::BTreeMap::from([("region".to_string(), "region".to_string())]),
        )];
        let (linked, skipped) = apply_variable_defaults(&conn, &pending).unwrap();
        assert_eq!(linked, 0);
        assert_eq!(skipped, 1);

        let vars = commands_repo::get_command_variables(&conn, "cmd1").unwrap();
        assert_eq!(vars[0].default_env_id, None);
    }

    #[test]
    fn apply_variable_defaults_skips_when_local_env_kind_is_incompatible() {
        let conn = open_in_memory().unwrap();
        insert_exported_command(
            &conn,
            exported_command_with_defaults("curl %t:region", "curl", &[("region", "region")]),
        )
        .unwrap();
        // A local env named "region" exists, but it's a secret - not
        // compatible with a %t: (text) variable.
        let env = build_stored_env(exported_env("region", "secret", "us-east"), Some(&test_key())).unwrap();
        env_repo::insert_env(&conn, &env).unwrap();

        let pending = vec![(
            "cmd1".to_string(),
            std::collections::BTreeMap::from([("region".to_string(), "region".to_string())]),
        )];
        let (linked, skipped) = apply_variable_defaults(&conn, &pending).unwrap();
        assert_eq!(linked, 0);
        assert_eq!(skipped, 1);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test commands::import::`
Expected: FAIL to compile (`apply_variable_defaults` doesn't exist)

- [ ] **Step 3: Implement**

Add to `src/commands/import.rs` (near the other free functions, e.g. after `build_stored_env`):

```rust
/// Applies each command's carried default-env settings (variable name ->
/// default env *name*) against the destination's own env table, linking by
/// name and skipping anything that doesn't resolve to a locally-present,
/// kind-compatible env. Returns (linked, skipped) counts for the summary
/// line. Deliberately not folded into `insert_exported_command` - this
/// runs as a separate pass after every command *and* env for this import
/// has already been written, since a default might reference an env that
/// arrives later in the same file.
fn apply_variable_defaults(
    conn: &rusqlite::Connection,
    pending: &[(String, std::collections::BTreeMap<String, String>)],
) -> Result<(usize, usize)> {
    let mut linked = 0;
    let mut skipped = 0;
    for (command_id, defaults) in pending {
        let vars = commands_repo::get_command_variables(conn, command_id)?;
        for (var_name, env_name) in defaults {
            let resolved = env_repo::find_by_name(conn, env_name)?.filter(|env| {
                vars.iter()
                    .any(|v| &v.var.name == var_name && env.kind.compatible_with(v.var.kind))
            });
            match resolved {
                Some(env) => {
                    commands_repo::set_default_env(conn, command_id, var_name, Some(&env.id))?;
                    linked += 1;
                }
                None => skipped += 1,
            }
        }
    }
    Ok((linked, skipped))
}
```

Now wire it into `run()`. Collect `(command_id, variable_defaults)` as commands are inserted/replaced, then apply after both commands and envs are settled. Replace the two command-insertion loops:

```rust
    for ec in new_commands {
        insert_exported_command(&tx, ec)?;
        imported_commands += 1;
    }
    for (conflict, use_incoming) in cmd_conflicts.into_iter().zip(cmd_resolutions) {
        if use_incoming {
            commands_repo::delete_command(&tx, &conflict.local.id)?;
            insert_exported_command(&tx, conflict.incoming)?;
            replaced_commands += 1;
        }
    }
```

with:

```rust
    let mut pending_defaults: Vec<(String, std::collections::BTreeMap<String, String>)> = Vec::new();

    for ec in new_commands {
        let command_id = ec.id.clone();
        let defaults = ec.variable_defaults.clone();
        insert_exported_command(&tx, ec)?;
        pending_defaults.push((command_id, defaults));
        imported_commands += 1;
    }
    for (conflict, use_incoming) in cmd_conflicts.into_iter().zip(cmd_resolutions) {
        if use_incoming {
            commands_repo::delete_command(&tx, &conflict.local.id)?;
            let command_id = conflict.incoming.id.clone();
            let defaults = conflict.incoming.variable_defaults.clone();
            insert_exported_command(&tx, conflict.incoming)?;
            pending_defaults.push((command_id, defaults));
            replaced_commands += 1;
        }
    }
```

Then, after the envs loops (right before `tx.commit()?;`), apply the pass:

```rust
    let (linked_defaults, skipped_defaults) = apply_variable_defaults(&tx, &pending_defaults)?;

    tx.commit()?;
```

Finally, extend the closing summary to mention it when relevant:

```rust
    println!(
        "Done. Imported {imported_commands} new command(s) and {imported_envs} new env(s); \
         replaced {replaced_commands} command(s) and {replaced_envs} env(s) with the incoming version; \
         kept {kept_commands} command(s) and {kept_envs} env(s) as-is."
    );
    if linked_defaults > 0 || skipped_defaults > 0 {
        println!(
            "Linked {linked_defaults} default env setting(s); skipped {skipped_defaults} \
             (no matching compatible env on this machine)."
        );
    }
    Ok(())
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo build && cargo test`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/commands/import.rs
git commit -m "$(cat <<'EOF'
Re-link default envs by name on inr import

A carried default is applied only to commands newly inserted or replaced
with the incoming version this run, and only when a compatible-kind env
exists locally under that name - otherwise it's silently skipped and
counted in the closing summary. Commands classified as identical are left
untouched even if the incoming file has a default they lack.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015erh8QLJbinntztAzonBBn
EOF
)"
```

---

## Task 8: README

**Files:**
- Modify: `README.md`

**Interfaces:**
- Consumes: nothing (documentation only).
- Produces: nothing consumed by later tasks.

- [ ] **Step 1: Update the command reference table**

In the `## Command reference` table, add a row for `inr e` right after the `inr d <id>` row:

```markdown
| `inr d <id>` | Delete a command by its id (asks for confirmation). |
| `inr e <id>` | Edit a command's description and text interactively; editing the text re-parses its `%s:`/`%t:`/`%n:` variables and asks to confirm if they changed. |
```

- [ ] **Step 2: Add a "Default envs" subsection**

Insert a new subsection after `## The `@` lookup` and before `## Sharing between machines`:

```markdown
## Default envs for variables

Any `%s:`/`%t:`/`%n:` variable on a command can have a default env: an env
whose value fills that variable automatically every time you run the
command with `inr s`, with no prompt at all. Set it right after `inr a`
saves a new command, or any time afterward via `inr e <id>` - both walk
each variable and let you pick a compatible-kind saved env, change it, or
clear it.

A secret variable with a default still requires your master password to
decrypt it (same as picking one manually with the secret `@` lookup) - it's
only the "type it or look it up?" prompt itself that's skipped.

If a default's env is later removed with `inr env r`, nothing breaks and
nothing is silently repointed: the next time you run that command, `inr s`
notices the default no longer resolves, prints a one-line notice, and
falls back to prompting for that variable exactly as if no default had
ever been set.
```

- [ ] **Step 3: Note default-env behavior in "Sharing between machines"**

In the existing `## Sharing between machines` section, add a bullet after the "Secrets are re-keyed, not copied" bullet:

```markdown
- **Default envs travel by name, not id.** If a variable has a default env
  set, `export` records that env's *name*; `import` re-links it against
  whatever env has that name on the destination, as long as it's still a
  compatible kind for that variable. If no matching env exists there, the
  default is simply left unset on import - the command itself still
  imports normally, and the closing summary reports how many default links
  were skipped.
```

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "$(cat <<'EOF'
Document inr e and default envs in README

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015erh8QLJbinntztAzonBBn
EOF
)"
```

---

## Task 9: Full verification pass

**Files:** none (verification only)

**Interfaces:** none

- [ ] **Step 1: Format check**

Run: `cargo fmt --check`
Expected: no diff. If there is one, run `cargo fmt` and review the diff before committing it separately.

- [ ] **Step 2: Build**

Run: `cargo build`
Expected: clean build, no warnings.

- [ ] **Step 3: Full test suite**

Run: `cargo test`
Expected: all tests pass, including every test added in Tasks 1-7.

- [ ] **Step 4: Clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: no warnings. Fix anything it flags (e.g. needless clones introduced by the `.clone()` calls in Task 7's `pending_defaults` collection) before moving on.

- [ ] **Step 5: Manual end-to-end smoke test**

Against a scratch vault, run through the full lifecycle once: `inr i` → `inr a "curl -H 'Authorization: %s:token' %t:host"` (set a default env for `host`, skip `token`) → `inr s` (confirm `host` fills silently, `token` still prompts) → `inr e <id>` (change the description only; confirm no variable-diff prompt appears) → `inr export scratch.inrx` → `inr import scratch.inrx` against a second scratch vault that already has a compatibly-named `host` env (confirm the "Linked N default env setting(s)" line appears and `inr s` on the imported command also auto-fills `host`).

This step has no automated pass/fail — record the outcome in the task's final commit message or PR description instead of a checkbox-only "done".
