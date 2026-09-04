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

/// Lazily unlocks and caches the master key for the duration of one `inr`
/// invocation, so running a command with several secret variables only
/// prompts for the password once.
pub struct KeyCache<'a> {
    conn: &'a Connection,
    key: Option<DerivedKey>,
}

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
        if self.key.is_none() {
            let cfg = db::load_config(self.conn)?
                .ok_or_else(|| anyhow!("inr is not initialized - run `inr i` first"))?;
            loop {
                let password = interactive::prompt_existing_master_password()?;
                match crypto::unlock(&password, &cfg.kdf_salt, &cfg.canary_nonce, &cfg.canary_cipher) {
                    Ok(key) => {
                        self.key = Some(key);
                        break;
                    }
                    Err(_) => println!("Wrong password, try again."),
                }
            }
        }
        Ok(self.key.as_ref().expect("just set"))
    }
}

fn compatible_env_kinds(cmd_kind: CmdVarKind) -> Vec<EnvKind> {
    match cmd_kind {
        CmdVarKind::Secret => vec![EnvKind::Secret],
        CmdVarKind::Text => vec![EnvKind::Text],
        CmdVarKind::Number => vec![EnvKind::NumberFloat, EnvKind::NumberInt],
    }
}

fn decrypt_env_value(keys: &mut KeyCache, env: &crate::models::StoredEnv) -> Result<Zeroizing<String>> {
    let nonce = env
        .nonce
        .as_ref()
        .ok_or_else(|| anyhow!("secret env '{}' is missing its nonce", env.name))?;
    let key = keys.get()?;
    let plaintext = crypto::decrypt(key, nonce, &env.value)?;
    let s = String::from_utf8(plaintext.to_vec())
        .map_err(|_| anyhow!("secret env '{}' is not valid UTF-8", env.name))?;
    Ok(Zeroizing::new(s))
}

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

fn resolve_literal_or_env_variable(
    conn: &Rc<Connection>,
    var: &CmdVariable,
) -> Result<Option<ResolvedVar>> {
    let kind_filter = compatible_env_kinds(var.kind);
    let message = format!(
        "{} ({}) - type a value, or '@' to search saved envs:",
        var.name, var.kind
    );

    loop {
        let answer = Text::new(&message)
            .with_autocomplete(EnvLookupAutocomplete::new(conn.clone(), kind_filter.clone()))
            .with_help_message("↑/↓ + Enter to pick a saved env, or just type a literal value")
            .prompt();

        let answer = match answer {
            Ok(a) => a,
            Err(InquireError::OperationCanceled) | Err(InquireError::OperationInterrupted) => {
                return Ok(None)
            }
            Err(e) => return Err(e.into()),
        };

        if let Some(id) = tui::parse_id_suffix(&answer) {
            let Some(env) = env_repo::get_by_id(conn, &id)? else {
                println!("That env no longer exists, try again.");
                continue;
            };
            // Defense in depth: the search was already kind-filtered, this
            // just guards against that filter ever being wrong - a secret
            // must never flow into a %t:/%n: slot, where it would end up in
            // argv/history in plaintext.
            debug_assert!(env.kind.compatible_with(var.kind));
            let value = String::from_utf8_lossy(&env.value).to_string();
            return Ok(Some(ResolvedVar {
                var: var.clone(),
                value: Zeroizing::new(value),
                source: BindingSource::Env(env.name.clone()),
            }));
        }

        if answer.trim_start().starts_with('@') {
            println!("Type to search, then use ↑/↓ and Enter to select a saved env.");
            continue;
        }

        if var.kind == CmdVarKind::Number && !interactive::validate_number(&answer) {
            println!("'{answer}' is not a valid number, try again.");
            continue;
        }

        return Ok(Some(ResolvedVar {
            var: var.clone(),
            value: Zeroizing::new(answer),
            source: BindingSource::Literal,
        }));
    }
}

fn resolve_secret_variable(
    conn: &Rc<Connection>,
    keys: &mut KeyCache,
    var: &CmdVariable,
) -> Result<Option<ResolvedVar>> {
    let choice = Select::new(
        &format!("{} (secret) - how do you want to provide it?", var.name),
        vec!["Type it", "Look up from saved secrets (@)"],
    )
    .prompt();

    let choice = match choice {
        Ok(c) => c,
        Err(InquireError::OperationCanceled) | Err(InquireError::OperationInterrupted) => {
            return Ok(None)
        }
        Err(e) => return Err(e.into()),
    };

    if choice == "Type it" {
        let value = Password::new(&format!("{}:", var.name))
            .with_display_mode(PasswordDisplayMode::Hidden)
            .without_confirmation()
            .prompt();
        let value = match value {
            Ok(v) => v,
            Err(InquireError::OperationCanceled) | Err(InquireError::OperationInterrupted) => {
                return Ok(None)
            }
            Err(e) => return Err(e.into()),
        };
        return Ok(Some(ResolvedVar {
            var: var.clone(),
            value: Zeroizing::new(value),
            source: BindingSource::Literal,
        }));
    }

    let Some(env) = tui::select_env(conn, &[EnvKind::Secret], "Search secrets:")? else {
        return Ok(None);
    };
    debug_assert!(env.kind.compatible_with(var.kind));
    let value = decrypt_env_value(keys, &env)?;
    Ok(Some(ResolvedVar {
        var: var.clone(),
        value,
        source: BindingSource::Env(env.name.clone()),
    }))
}

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
