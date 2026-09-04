use crate::crypto::{self, DerivedKey};
use crate::db::{self, env_repo};
use crate::interactive;
use crate::models::{CmdVarKind, CmdVariable, EnvKind};
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
/// if the user cancelled (Esc).
pub fn resolve_variable(
    conn: &Rc<Connection>,
    keys: &mut KeyCache,
    var: &CmdVariable,
) -> Result<Option<ResolvedVar>> {
    match var.kind {
        CmdVarKind::Secret => resolve_secret_variable(conn, keys, var),
        CmdVarKind::Text | CmdVarKind::Number => resolve_literal_or_env_variable(conn, var),
    }
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
