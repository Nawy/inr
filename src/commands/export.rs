use crate::db::{self, commands_repo, env_repo};
use crate::models::EnvKind;
use crate::transfer::{ExportedCommand, ExportedEnv, TransferPayload};
use crate::{crypto, interactive, paths, transfer};
use anyhow::{anyhow, Result};
use chrono::Utc;

pub fn run(path: String) -> Result<()> {
    let conn = db::open(paths::db_path()?)?;

    let commands = commands_repo::list_all(&conn)?;
    let envs = env_repo::list_all(&conn)?;

    let key = if envs.iter().any(|e| e.kind == EnvKind::Secret) {
        let cfg = db::load_config(&conn)?
            .ok_or_else(|| anyhow!("inr is not initialized - run `inr i` first"))?;
        let password =
            interactive::prompt_existing_password("Master password (to decrypt secrets for export):")?;
        Some(crypto::unlock(
            &password,
            &cfg.kdf_salt,
            &cfg.canary_nonce,
            &cfg.canary_cipher,
        )?)
    } else {
        None
    };

    let mut exported_envs = Vec::with_capacity(envs.len());
    for env in &envs {
        let value = match env.kind {
            EnvKind::Secret => {
                let nonce = env
                    .nonce
                    .as_ref()
                    .ok_or_else(|| anyhow!("secret env '{}' is missing its nonce", env.name))?;
                let key = key.as_ref().expect("key was derived because a secret env exists");
                let plaintext = crypto::decrypt(key, nonce, &env.value)?;
                String::from_utf8(plaintext.to_vec())
                    .map_err(|_| anyhow!("secret env '{}' is not valid UTF-8", env.name))?
            }
            _ => String::from_utf8_lossy(&env.value).to_string(),
        };
        exported_envs.push(ExportedEnv {
            id: env.id.clone(),
            name: env.name.clone(),
            kind: env.kind.as_str().to_string(),
            value,
            description: env.description.clone(),
            created_at: env.created_at.clone(),
            updated_at: env.updated_at.clone(),
        });
    }

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

    let payload = TransferPayload {
        exported_at: Utc::now().to_rfc3339(),
        commands: exported_commands,
        envs: exported_envs,
    };

    println!(
        "Exporting {} command(s) and {} env(s).",
        payload.commands.len(),
        payload.envs.len()
    );

    let passphrase =
        interactive::prompt_new_password("Transfer passphrase (protects this file - not your master password):")?;

    let bytes = transfer::encode(&payload, &passphrase)?;
    std::fs::write(&path, bytes)?;

    println!(
        "Wrote {path} ({} command(s), {} env(s)). Keep the transfer passphrase safe - it's needed to import this file.",
        payload.commands.len(),
        payload.envs.len()
    );
    Ok(())
}
