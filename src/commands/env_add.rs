use crate::db::{self, env_repo};
use crate::models::{parse_env_spec, EnvKind, StoredEnv};
use crate::{crypto, interactive, paths};
use anyhow::{anyhow, Result};
use chrono::Utc;
use inquire::{Password, PasswordDisplayMode};
use nanoid::nanoid;

pub fn run(spec: String) -> Result<()> {
    let conn = db::open(paths::db_path()?)?;
    if !db::is_initialized(&conn)? {
        return Err(anyhow!("inr is not initialized - run `inr i` first"));
    }

    let (kind, name) = parse_env_spec(&spec);
    if name.is_empty() {
        return Err(anyhow!("env name cannot be empty"));
    }

    if let Some(existing) = env_repo::find_by_name(&conn, &name)? {
        return Err(anyhow!("'{name}' already exists (id: {})", existing.id));
    }

    let description = interactive::prompt_text("Description (searchable):")?;

    let (value, nonce): (Vec<u8>, Option<Vec<u8>>) = match kind {
        EnvKind::Secret => {
            let cfg = db::load_config(&conn)?
                .ok_or_else(|| anyhow!("inr is not initialized - run `inr i` first"))?;
            let password = interactive::prompt_existing_master_password()?;
            let key = crypto::unlock(
                &password,
                &cfg.kdf_salt,
                &cfg.canary_nonce,
                &cfg.canary_cipher,
            )?;
            let secret_value = Password::new("Value:")
                .with_display_mode(PasswordDisplayMode::Hidden)
                .without_confirmation()
                .prompt()?;
            let (nonce, cipher) = crypto::encrypt(&key, secret_value.as_bytes())?;
            (cipher, Some(nonce.to_vec()))
        }
        EnvKind::Text => {
            let value = interactive::prompt_text("Value:")?;
            (value.into_bytes(), None)
        }
        EnvKind::NumberFloat => {
            let value = loop {
                let v = interactive::prompt_text("Value:")?;
                if v.parse::<f64>().is_ok() {
                    break v;
                }
                println!("'{v}' is not a valid float.");
            };
            (value.into_bytes(), None)
        }
        EnvKind::NumberInt => {
            let value = loop {
                let v = interactive::prompt_text("Value:")?;
                if v.parse::<i64>().is_ok() {
                    break v;
                }
                println!("'{v}' is not a valid integer.");
            };
            (value.into_bytes(), None)
        }
    };

    let now = Utc::now().to_rfc3339();
    let id = nanoid!(6);
    let env = StoredEnv {
        id: id.clone(),
        name,
        kind,
        value,
        nonce,
        description,
        created_at: now.clone(),
        updated_at: now,
    };
    env_repo::insert_env(&conn, &env)?;
    println!("Saved. id: {id}");
    Ok(())
}
