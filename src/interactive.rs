use anyhow::Result;
use inquire::validator::Validation;
use inquire::{Confirm, InquireError, Password, PasswordDisplayMode, Select, Text};

/// Prompts for a brand-new password under `message`, with inquire's built-in
/// confirmation flow (asks twice, re-prompts on mismatch) and a minimum
/// length check. Used for both the master password (`inr i`) and a
/// transfer-file passphrase (`inr export`).
pub fn prompt_new_password(message: &str) -> Result<String> {
    let password = Password::new(message)
        .with_display_mode(PasswordDisplayMode::Hidden)
        .with_validator(|input: &str| {
            if input.chars().count() < 8 {
                Ok(Validation::Invalid("Must be at least 8 characters.".into()))
            } else {
                Ok(Validation::Valid)
            }
        })
        .with_custom_confirmation_message("Confirm:")
        .with_custom_confirmation_error_message("Passwords did not match, try again.")
        .prompt()?;
    Ok(password)
}

/// Prompts for an existing password under `message` (no confirmation, no
/// length check - correctness is verified separately, e.g. against a
/// stored canary or by a failed decrypt).
pub fn prompt_existing_password(message: &str) -> Result<String> {
    let password = Password::new(message)
        .with_display_mode(PasswordDisplayMode::Hidden)
        .without_confirmation()
        .prompt()?;
    Ok(password)
}

pub fn prompt_new_master_password() -> Result<String> {
    prompt_new_password("Master password:")
}

/// Prompts for the existing master password (no confirmation, no length
/// check - correctness is verified separately against the stored canary).
pub fn prompt_existing_master_password() -> Result<String> {
    prompt_existing_password("Master password:")
}

pub fn confirm(message: &str, default: bool) -> Result<bool> {
    let answer = Confirm::new(message).with_default(default).prompt()?;
    Ok(answer)
}

/// Free-text entry, no autocomplete, no masking - used for descriptions and
/// plain text/number literal values.
pub fn prompt_text(message: &str) -> Result<String> {
    let answer = Text::new(message).prompt()?;
    Ok(answer)
}

pub fn validate_number(input: &str) -> bool {
    input.parse::<f64>().is_ok()
}

/// Runs an inquire `Select` menu among `options`, returning `None` if the
/// user cancels (Esc/Ctrl+C) instead of propagating an error - used for
/// `inr import`'s conflict-resolution menus and per-conflict pickers.
pub fn select_one(message: &str, options: Vec<&'static str>) -> Result<Option<&'static str>> {
    match Select::new(message, options).prompt() {
        Ok(choice) => Ok(Some(choice)),
        Err(InquireError::OperationCanceled) | Err(InquireError::OperationInterrupted) => Ok(None),
        Err(e) => Err(e.into()),
    }
}
