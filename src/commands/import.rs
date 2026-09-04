use crate::crypto::DerivedKey;
use crate::db::{self, commands_repo, env_repo};
use crate::models::{EnvKind, StoredCommand, StoredEnv};
use crate::transfer::{ExportedCommand, ExportedEnv, TransferPayload};
use crate::{crypto, interactive, paths, placeholder, transfer};
use anyhow::{anyhow, Context, Result};

/// A command whose template matches a local one, but whose content differs.
struct CommandConflict {
    local: StoredCommand,
    incoming: ExportedCommand,
}

/// An env whose name matches a local one, but whose content differs.
/// `local_value` is the local value in comparable plaintext form (a secret
/// env's value has already been decrypted with the destination key).
struct EnvConflict {
    local: StoredEnv,
    local_value: String,
    incoming: ExportedEnv,
}

pub fn run(path: String) -> Result<()> {
    let mut conn = db::open(paths::db_path()?)?;

    let bytes = std::fs::read(&path).with_context(|| format!("failed to read {path}"))?;
    let passphrase = interactive::prompt_existing_password("Transfer passphrase:")?;
    let payload = transfer::decode(&bytes, &passphrase)?;

    let dest_key = unlock_destination_if_needed(&conn, &payload)?;

    let (new_commands, cmd_conflicts, identical_commands) = diff_commands(&conn, payload.commands)?;
    let (new_envs, env_conflicts, identical_envs) =
        diff_envs(&conn, payload.envs, dest_key.as_ref())?;

    println!(
        "{} new command(s), {} new env(s), {} conflicting command(s), {} conflicting env(s), {} unchanged.",
        new_commands.len(),
        new_envs.len(),
        cmd_conflicts.len(),
        env_conflicts.len(),
        identical_commands + identical_envs
    );

    let total_conflicts = cmd_conflicts.len() + env_conflicts.len();
    let Some((cmd_resolutions, env_resolutions)) =
        resolve_conflicts(&cmd_conflicts, &env_conflicts, total_conflicts)?
    else {
        println!("Cancelled. No changes made.");
        return Ok(());
    };

    let kept_commands = cmd_conflicts.len() - cmd_resolutions.iter().filter(|&&r| r).count();
    let kept_envs = env_conflicts.len() - env_resolutions.iter().filter(|&&r| r).count();

    let tx = conn.transaction()?;
    let mut imported_commands = 0;
    let mut imported_envs = 0;
    let mut replaced_commands = 0;
    let mut replaced_envs = 0;

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

    for ee in new_envs {
        let env = build_stored_env(ee, dest_key.as_ref())?;
        env_repo::insert_env(&tx, &env)?;
        imported_envs += 1;
    }
    for (conflict, use_incoming) in env_conflicts.into_iter().zip(env_resolutions) {
        if use_incoming {
            env_repo::delete_env(&tx, &conflict.local.id)?;
            let env = build_stored_env(conflict.incoming, dest_key.as_ref())?;
            env_repo::insert_env(&tx, &env)?;
            replaced_envs += 1;
        }
    }

    tx.commit()?;

    println!(
        "Done. Imported {imported_commands} new command(s) and {imported_envs} new env(s); \
         replaced {replaced_commands} command(s) and {replaced_envs} env(s) with the incoming version; \
         kept {kept_commands} command(s) and {kept_envs} env(s) as-is."
    );
    Ok(())
}

/// If the file contains any secret env, the destination needs its own
/// master key both to compare against existing local secrets (during diff)
/// and to re-encrypt incoming secret values (during insert) - ciphertext
/// from the source machine can never just be copied over, since it was
/// encrypted under a different key.
fn unlock_destination_if_needed(
    conn: &rusqlite::Connection,
    payload: &TransferPayload,
) -> Result<Option<DerivedKey>> {
    if !payload.envs.iter().any(|e| e.kind == EnvKind::Secret.as_str()) {
        return Ok(None);
    }

    let cfg = db::load_config(conn)?.ok_or_else(|| {
        anyhow!("inr is not initialized - run `inr i` first before importing secrets")
    })?;
    let password = interactive::prompt_existing_password("Destination master password:")?;
    let key = crypto::unlock(&password, &cfg.kdf_salt, &cfg.canary_nonce, &cfg.canary_cipher)?;
    Ok(Some(key))
}

type CommandDiff = (Vec<ExportedCommand>, Vec<CommandConflict>, usize);

/// Commands are matched by exact template text - `id` is a random nanoid
/// generated independently on each machine, so it's not a usable key here.
fn diff_commands(conn: &rusqlite::Connection, commands: Vec<ExportedCommand>) -> Result<CommandDiff> {
    let mut new_commands = Vec::new();
    let mut conflicts = Vec::new();
    let mut identical = 0;

    for ec in commands {
        match commands_repo::find_by_template(conn, &ec.template)? {
            None => new_commands.push(ec),
            Some(local) => {
                if local.description == ec.description {
                    identical += 1;
                } else {
                    conflicts.push(CommandConflict { local, incoming: ec });
                }
            }
        }
    }
    Ok((new_commands, conflicts, identical))
}

type EnvDiff = (Vec<ExportedEnv>, Vec<EnvConflict>, usize);

/// Envs are matched by name (the schema already enforces unique names).
fn diff_envs(
    conn: &rusqlite::Connection,
    envs: Vec<ExportedEnv>,
    dest_key: Option<&DerivedKey>,
) -> Result<EnvDiff> {
    let mut new_envs = Vec::new();
    let mut conflicts = Vec::new();
    let mut identical = 0;

    for ee in envs {
        match env_repo::find_by_name(conn, &ee.name)? {
            None => new_envs.push(ee),
            Some(local) => {
                let local_value = comparable_local_value(&local, dest_key)?;
                let unchanged = local.kind.as_str() == ee.kind
                    && local_value == ee.value
                    && local.description == ee.description;
                if unchanged {
                    identical += 1;
                } else {
                    conflicts.push(EnvConflict {
                        local,
                        local_value,
                        incoming: ee,
                    });
                }
            }
        }
    }
    Ok((new_envs, conflicts, identical))
}

/// Renders a local env's value as plaintext for comparison against an
/// incoming (already-plaintext) value - decrypting it with the destination
/// key if it's a secret.
fn comparable_local_value(local: &StoredEnv, dest_key: Option<&DerivedKey>) -> Result<String> {
    if local.kind != EnvKind::Secret {
        return Ok(String::from_utf8_lossy(&local.value).to_string());
    }
    let nonce = local
        .nonce
        .as_ref()
        .ok_or_else(|| anyhow!("secret env '{}' is missing its nonce", local.name))?;
    let key = dest_key.expect("destination key was unlocked because a secret env exists");
    let plaintext = crypto::decrypt(key, nonce, &local.value)?;
    Ok(String::from_utf8_lossy(&plaintext).to_string())
}

/// Returns `None` if the user cancelled instead of picking a resolution for
/// every conflict. Otherwise returns one bool per command conflict and one
/// per env conflict (`true` = use incoming), in the same order as the
/// conflict lists passed in.
fn resolve_conflicts(
    cmd_conflicts: &[CommandConflict],
    env_conflicts: &[EnvConflict],
    total_conflicts: usize,
) -> Result<Option<(Vec<bool>, Vec<bool>)>> {
    if total_conflicts == 0 {
        return Ok(Some((Vec::new(), Vec::new())));
    }

    let Some(choice) = interactive::select_one(
        "How do you want to resolve conflicts?",
        vec!["Resolve one by one", "Keep mine for all", "Use incoming for all"],
    )?
    else {
        return Ok(None);
    };

    match choice {
        "Keep mine for all" => {
            let message =
                format!("Keep your local version for all {total_conflicts} conflict(s)? Incoming changes will be discarded.");
            if !interactive::confirm(&message, false)? {
                return Ok(None);
            }
            Ok(Some((
                vec![false; cmd_conflicts.len()],
                vec![false; env_conflicts.len()],
            )))
        }
        "Use incoming for all" => {
            let message = format!(
                "Overwrite all {total_conflicts} conflict(s) with the incoming version from the file?"
            );
            if !interactive::confirm(&message, false)? {
                return Ok(None);
            }
            Ok(Some((
                vec![true; cmd_conflicts.len()],
                vec![true; env_conflicts.len()],
            )))
        }
        _ => resolve_one_by_one(cmd_conflicts, env_conflicts),
    }
}

fn resolve_one_by_one(
    cmd_conflicts: &[CommandConflict],
    env_conflicts: &[EnvConflict],
) -> Result<Option<(Vec<bool>, Vec<bool>)>> {
    let mut cmd_resolutions = Vec::with_capacity(cmd_conflicts.len());
    for conflict in cmd_conflicts {
        println!("\nCommand conflict:");
        println!(
            "  Mine:     {}  ({})",
            conflict.local.template, conflict.local.description
        );
        println!(
            "  Incoming: {}  ({})",
            conflict.incoming.template, conflict.incoming.description
        );
        let Some(pick) =
            interactive::select_one("Which version do you want to keep?", vec!["Keep mine", "Use incoming"])?
        else {
            return Ok(None);
        };
        cmd_resolutions.push(pick == "Use incoming");
    }

    let mut env_resolutions = Vec::with_capacity(env_conflicts.len());
    for conflict in env_conflicts {
        println!("\nEnv conflict: '{}'", conflict.local.name);
        let is_secret = conflict.local.kind == EnvKind::Secret || conflict.incoming.kind == "secret";
        if is_secret {
            println!(
                "  Mine:     kind={} description=\"{}\" value=(hidden)",
                conflict.local.kind, conflict.local.description
            );
            println!(
                "  Incoming: kind={} description=\"{}\" value=(hidden) -- values differ",
                conflict.incoming.kind, conflict.incoming.description
            );
        } else {
            println!(
                "  Mine:     kind={} value=\"{}\" description=\"{}\"",
                conflict.local.kind, conflict.local_value, conflict.local.description
            );
            println!(
                "  Incoming: kind={} value=\"{}\" description=\"{}\"",
                conflict.incoming.kind, conflict.incoming.value, conflict.incoming.description
            );
        }
        let Some(pick) =
            interactive::select_one("Which version do you want to keep?", vec!["Keep mine", "Use incoming"])?
        else {
            return Ok(None);
        };
        env_resolutions.push(pick == "Use incoming");
    }

    Ok(Some((cmd_resolutions, env_resolutions)))
}

fn insert_exported_command(conn: &rusqlite::Connection, ec: ExportedCommand) -> Result<()> {
    let vars = placeholder::parse_placeholders(&ec.template)?;
    let cmd = StoredCommand {
        id: ec.id,
        template: ec.template,
        description: ec.description,
        created_at: ec.created_at,
        updated_at: ec.updated_at,
    };
    commands_repo::insert_command_raw(conn, &cmd, &vars)
}

/// Builds a `StoredEnv` ready to insert, re-encrypting the value under the
/// destination's master key if it's a secret.
fn build_stored_env(ee: ExportedEnv, dest_key: Option<&DerivedKey>) -> Result<StoredEnv> {
    let kind = EnvKind::from_str(&ee.kind)?;
    let (value, nonce) = match kind {
        EnvKind::Secret => {
            let key = dest_key.expect("destination key was unlocked because a secret env exists");
            let (nonce, cipher) = crypto::encrypt(key, ee.value.as_bytes())?;
            (cipher, Some(nonce.to_vec()))
        }
        _ => (ee.value.into_bytes(), None),
    };
    Ok(StoredEnv {
        id: ee.id,
        name: ee.name,
        kind,
        value,
        nonce,
        description: ee.description,
        created_at: ee.created_at,
        updated_at: ee.updated_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

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

    fn exported_env(name: &str, kind: &str, value: &str) -> ExportedEnv {
        ExportedEnv {
            id: "env1".to_string(),
            name: name.to_string(),
            kind: kind.to_string(),
            value: value.to_string(),
            description: "desc".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn diff_commands_classifies_new_identical_and_conflicting() {
        let conn = open_in_memory().unwrap();
        insert_exported_command(&conn, exported_command("echo %t:msg", "say hi")).unwrap();

        let incoming = vec![
            exported_command("echo %t:msg", "say hi"),   // identical
            exported_command("echo %t:msg", "greet"),    // conflict (same template, diff desc)
            exported_command("ls -la", "list files"),    // new
        ];

        let (new_cmds, conflicts, identical) = diff_commands(&conn, incoming).unwrap();
        assert_eq!(new_cmds.len(), 1);
        assert_eq!(new_cmds[0].template, "ls -la");
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].incoming.description, "greet");
        assert_eq!(identical, 1);
    }

    #[test]
    fn diff_envs_classifies_new_identical_and_conflicting_for_plain_envs() {
        let conn = open_in_memory().unwrap();
        let env = build_stored_env(exported_env("username", "text", "alice"), None).unwrap();
        env_repo::insert_env(&conn, &env).unwrap();

        let incoming = vec![
            exported_env("username", "text", "alice"), // identical
            exported_env("username", "text", "bob"),   // this alone would conflict, but name repeats
        ];
        // Split into two separate diff calls to avoid a same-name payload,
        // which isn't a case `inr export` ever produces.
        let (new1, conflicts1, identical1) = diff_envs(&conn, vec![incoming[0].clone()], None).unwrap();
        assert!(new1.is_empty());
        assert!(conflicts1.is_empty());
        assert_eq!(identical1, 1);

        let (new2, conflicts2, identical2) = diff_envs(&conn, vec![incoming[1].clone()], None).unwrap();
        assert!(new2.is_empty());
        assert_eq!(conflicts2.len(), 1);
        assert_eq!(conflicts2[0].local_value, "alice");
        assert_eq!(conflicts2[0].incoming.value, "bob");
        assert_eq!(identical2, 0);

        let (new3, _, _) = diff_envs(&conn, vec![exported_env("apitoken", "text", "x")], None).unwrap();
        assert_eq!(new3.len(), 1);
    }

    #[test]
    fn diff_envs_compares_decrypted_secret_values() {
        let conn = open_in_memory().unwrap();
        let salt = crypto::random_salt();
        let key = crypto::derive_key("dest-master-password", &salt).unwrap();
        let env = build_stored_env(exported_env("apikey", "secret", "sk-old"), Some(&key)).unwrap();
        env_repo::insert_env(&conn, &env).unwrap();

        let (_, conflicts, identical) = diff_envs(
            &conn,
            vec![exported_env("apikey", "secret", "sk-new")],
            Some(&key),
        )
        .unwrap();
        assert_eq!(identical, 0);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].local_value, "sk-old");

        let (_, conflicts, identical) = diff_envs(
            &conn,
            vec![exported_env("apikey", "secret", "sk-old")],
            Some(&key),
        )
        .unwrap();
        assert_eq!(identical, 1);
        assert!(conflicts.is_empty());
    }

    #[test]
    fn build_stored_env_encrypts_secrets_and_leaves_plain_kinds_untouched() {
        let salt = crypto::random_salt();
        let key = crypto::derive_key("pw", &salt).unwrap();

        let secret_env = build_stored_env(exported_env("apikey", "secret", "sk-live-123"), Some(&key)).unwrap();
        assert_ne!(secret_env.value, b"sk-live-123".to_vec());
        assert!(secret_env.nonce.is_some());
        let decrypted = crypto::decrypt(&key, secret_env.nonce.as_ref().unwrap(), &secret_env.value).unwrap();
        assert_eq!(decrypted.as_slice(), b"sk-live-123");

        let text_env = build_stored_env(exported_env("username", "text", "alice"), None).unwrap();
        assert_eq!(text_env.value, b"alice".to_vec());
        assert!(text_env.nonce.is_none());
    }

    #[test]
    fn insert_exported_command_derives_variables_from_template() {
        let conn = open_in_memory().unwrap();
        insert_exported_command(&conn, exported_command("ssh %s:host -p %n:port", "connect")).unwrap();

        let vars = commands_repo::get_command_variables(&conn, "cmd1").unwrap();
        assert_eq!(vars.len(), 2);
        assert_eq!(vars[0].var.name, "host");
        assert_eq!(vars[1].var.name, "port");
    }
}
