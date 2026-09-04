use crate::db::{self};
use crate::models::EnvKind;
use crate::{crypto, interactive, paths, tui};
use anyhow::Result;
use crossterm::event;
use crossterm::terminal;
use std::rc::Rc;
use std::time::{Duration, Instant};

const CLIPBOARD_CLEAR_AFTER: Duration = Duration::from_secs(20);

pub fn run() -> Result<()> {
    let conn = Rc::new(db::open(paths::db_path()?)?);

    let Some(env) = tui::select_env(&conn, &[], "Search envs:")? else {
        println!("Cancelled.");
        return Ok(());
    };

    let value = match env.kind {
        EnvKind::Secret => {
            let cfg = db::load_config(&conn)?
                .ok_or_else(|| anyhow::anyhow!("inr is not initialized - run `inr i` first"))?;
            let password = interactive::prompt_existing_master_password()?;
            let key = crypto::unlock(
                &password,
                &cfg.kdf_salt,
                &cfg.canary_nonce,
                &cfg.canary_cipher,
            )?;
            let nonce = env
                .nonce
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("secret env is missing its nonce"))?;
            let plaintext = crypto::decrypt(&key, nonce, &env.value)?;
            String::from_utf8(plaintext.to_vec())
                .map_err(|_| anyhow::anyhow!("secret is not valid UTF-8"))?
        }
        _ => String::from_utf8_lossy(&env.value).to_string(),
    };

    let mut clipboard = arboard::Clipboard::new()?;
    clipboard.set_text(value.clone())?;

    if env.kind == EnvKind::Secret {
        println!(
            "Copied to clipboard. Clearing in {}s (press any key to clear now)...",
            CLIPBOARD_CLEAR_AFTER.as_secs()
        );
        wait_then_clear(&mut clipboard, &value)?;
        println!("Clipboard cleared.");
    } else {
        println!("Copied to clipboard.");
    }

    Ok(())
}

fn wait_then_clear(clipboard: &mut arboard::Clipboard, expected: &str) -> Result<()> {
    let deadline = Instant::now() + CLIPBOARD_CLEAR_AFTER;
    let raw_mode_enabled = terminal::enable_raw_mode().is_ok();

    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let poll_for = remaining.min(Duration::from_millis(200));
        if event::poll(poll_for).unwrap_or(false) {
            let _ = event::read();
            break;
        }
    }

    if raw_mode_enabled {
        let _ = terminal::disable_raw_mode();
    }

    // Only clear if the clipboard still holds exactly what we put there -
    // don't clobber something the user copied elsewhere in the meantime.
    if let Ok(current) = clipboard.get_text() {
        if current == expected {
            let _ = clipboard.set_text(String::new());
        }
    }
    Ok(())
}
