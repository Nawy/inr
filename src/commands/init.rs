use crate::{crypto, db, interactive, paths};
use anyhow::{anyhow, Result};
use chrono::Utc;

pub fn run() -> Result<()> {
    let path = paths::db_path()?;
    let conn = db::open(&path)?;

    if db::is_initialized(&conn)? {
        return Err(anyhow!("inr is already initialized at {}", path.display()));
    }

    println!("Setting up inr at {}", path.display());
    let password = interactive::prompt_new_master_password()?;

    let salt = crypto::random_salt();
    let key = crypto::derive_key(&password, &salt)?;
    let (canary_nonce, canary_cipher) = crypto::create_canary(&key)?;

    let cfg = db::Config {
        kdf_salt: salt.to_vec(),
        canary_nonce: canary_nonce.to_vec(),
        canary_cipher,
    };
    db::save_config(&conn, &cfg, &Utc::now().to_rfc3339())?;

    println!(
        "inr initialized. Secrets are protected by your master password - there is no recovery if it's lost."
    );
    Ok(())
}
