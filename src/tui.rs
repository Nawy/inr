use crate::db::{commands_repo, env_repo};
use crate::models::{EnvKind, StoredCommand, StoredEnv};
use anyhow::Result;
use crossterm::style::{Color, Stylize};
use inquire::{autocompletion::Replacement, Autocomplete, CustomUserError, InquireError, Text};
use regex::Regex;
use rusqlite::Connection;
use std::rc::Rc;
use std::sync::LazyLock;

static ID_SUFFIX_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[([A-Za-z0-9_-]+)\]\s*$").unwrap());

/// The color used for the field you're actually scanning for in a
/// suggestion list - a command's description, an env's name - so it jumps
/// out from the dimmer reference detail (command text, value, id) next to
/// it.
const HIGHLIGHT_COLOR: Color = Color::Rgb {
    r: 255,
    g: 140,
    b: 0,
};

/// Max rows returned by either search-as-you-type query, applied as a SQL
/// LIMIT - the suggestion list can never grow past this regardless of how
/// many commands/envs are stored or how tall the terminal is.
const MAX_SUGGESTIONS: i64 = 10;

fn to_custom_err(e: anyhow::Error) -> CustomUserError {
    Box::<dyn std::error::Error + Send + Sync>::from(e.to_string())
}

pub fn parse_id_suffix(suggestion: &str) -> Option<String> {
    ID_SUFFIX_RE
        .captures(suggestion)
        .map(|c| c[1].to_string())
}

/// Description in orange, the command itself dimmed to grey - the
/// description is what you're scanning for, the command is reference detail
/// once you've found it. Inquire's renderer is ANSI-aware, so these embedded
/// escape codes render correctly and are excluded from width/cursor math;
/// the trailing `[id]` stays unstyled so `parse_id_suffix` keeps matching
/// plain text.
pub fn format_command_suggestion(cmd: &StoredCommand) -> String {
    format!(
        "{} {} {}   [{}]",
        cmd.description.as_str().with(HIGHLIGHT_COLOR).bold(),
        "→".dark_grey(),
        cmd.template.as_str().dark_grey(),
        cmd.id
    )
}

/// A value preview shown next to an env's name in a suggestion list. Never
/// reveals a secret's plaintext - only ever shows a masked placeholder for
/// Secret kind, matching `inr env s`'s display rule.
pub fn env_display_value(env: &StoredEnv) -> String {
    match env.kind {
        EnvKind::Secret => "***".to_string(),
        _ => String::from_utf8_lossy(&env.value).to_string(),
    }
}

pub fn format_env_suggestion(env: &StoredEnv) -> String {
    format!(
        "{} {} {}  {}   [{}]",
        env.name.as_str().with(HIGHLIGHT_COLOR).bold(),
        "=".dark_grey(),
        env_display_value(env).dark_grey(),
        format!("({})", env.kind).dark_grey(),
        env.id
    )
}

#[derive(Clone)]
pub struct CommandAutocomplete {
    conn: Rc<Connection>,
}

impl CommandAutocomplete {
    pub fn new(conn: Rc<Connection>) -> Self {
        Self { conn }
    }
}

impl Autocomplete for CommandAutocomplete {
    fn get_suggestions(&mut self, input: &str) -> Result<Vec<String>, CustomUserError> {
        let results = commands_repo::search_commands(&self.conn, input, MAX_SUGGESTIONS)
            .map_err(to_custom_err)?;
        Ok(results.iter().map(format_command_suggestion).collect())
    }

    fn get_completion(
        &mut self,
        _input: &str,
        highlighted: Option<String>,
    ) -> Result<Replacement, CustomUserError> {
        Ok(highlighted)
    }
}

/// Autocompletes env names, restricted to `kind_filter` (empty = no
/// restriction).
///
/// In `Deferred` mode it only activates once the input starts with '@' -
/// used for a combined "type a literal value, or @ to search" field where
/// plain typing must NOT be treated as a search query. In `Immediate` mode
/// it searches on every keystroke with no prefix required - used for a
/// field whose sole purpose is already "search envs" (`inr env s`, and the
/// secret lookup after choosing "Look up from saved secrets" in `inr s`),
/// where making the user type '@' first would just be a redundant step.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EnvLookupMode {
    Immediate,
    Deferred,
}

#[derive(Clone)]
pub struct EnvLookupAutocomplete {
    conn: Rc<Connection>,
    kind_filter: Vec<EnvKind>,
    mode: EnvLookupMode,
}

impl EnvLookupAutocomplete {
    /// Requires the input to start with '@' before searching.
    pub fn new(conn: Rc<Connection>, kind_filter: Vec<EnvKind>) -> Self {
        Self {
            conn,
            kind_filter,
            mode: EnvLookupMode::Deferred,
        }
    }

    /// Searches immediately, no '@' prefix required.
    pub fn new_immediate(conn: Rc<Connection>, kind_filter: Vec<EnvKind>) -> Self {
        Self {
            conn,
            kind_filter,
            mode: EnvLookupMode::Immediate,
        }
    }
}

impl Autocomplete for EnvLookupAutocomplete {
    fn get_suggestions(&mut self, input: &str) -> Result<Vec<String>, CustomUserError> {
        let query = match self.mode {
            EnvLookupMode::Immediate => input,
            EnvLookupMode::Deferred => match input.strip_prefix('@') {
                Some(q) => q,
                None => return Ok(vec![]),
            },
        };
        let results =
            env_repo::search_envs(&self.conn, query, &self.kind_filter, MAX_SUGGESTIONS)
                .map_err(to_custom_err)?;
        Ok(results.iter().map(format_env_suggestion).collect())
    }

    fn get_completion(
        &mut self,
        _input: &str,
        highlighted: Option<String>,
    ) -> Result<Replacement, CustomUserError> {
        Ok(highlighted)
    }
}

/// Runs the interactive FTS5 search-as-you-type prompt for commands and
/// returns the selected command, or `None` if the user cancelled (Esc).
/// Loops with an error message if the submitted text isn't an actual
/// selection from the list (arrow keys + Enter required).
pub fn select_command(conn: &Rc<Connection>) -> Result<Option<StoredCommand>> {
    loop {
        let answer = Text::new("Search commands:")
            .with_autocomplete(CommandAutocomplete::new(conn.clone()))
            .with_help_message("Type to search, ↑/↓ to select, Enter to run, Esc to cancel")
            .prompt();

        let answer = match answer {
            Ok(a) => a,
            Err(InquireError::OperationCanceled) | Err(InquireError::OperationInterrupted) => {
                return Ok(None)
            }
            Err(e) => return Err(e.into()),
        };

        let Some(id) = parse_id_suffix(&answer) else {
            println!("No command selected - pick one from the list with ↑/↓ and Enter.");
            continue;
        };

        if let Some(cmd) = commands_repo::get_command(conn, &id)? {
            return Ok(Some(cmd));
        }
        println!("That command no longer exists, try again.");
    }
}

/// Same as `select_command` but for envs, restricted to `kind_filter`
/// (empty = all kinds). Used by both `inr env s` (no filter) and the `@`
/// lookup inside a command variable prompt (filtered to compatible kinds).
pub fn select_env(
    conn: &Rc<Connection>,
    kind_filter: &[EnvKind],
    message: &str,
) -> Result<Option<StoredEnv>> {
    loop {
        let answer = Text::new(message)
            .with_autocomplete(EnvLookupAutocomplete::new_immediate(
                conn.clone(),
                kind_filter.to_vec(),
            ))
            .with_help_message("Type to search, ↑/↓ to select, Enter to confirm, Esc to cancel")
            .prompt();

        let answer = match answer {
            Ok(a) => a,
            Err(InquireError::OperationCanceled) | Err(InquireError::OperationInterrupted) => {
                return Ok(None)
            }
            Err(e) => return Err(e.into()),
        };

        let Some(id) = parse_id_suffix(&answer) else {
            println!("No env selected - pick one from the list with ↑/↓ and Enter.");
            continue;
        };

        if let Some(env) = env_repo::get_by_id(conn, &id)? {
            return Ok(Some(env));
        }
        println!("That env no longer exists, try again.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_id_suffix_from_formatted_suggestion() {
        assert_eq!(
            parse_id_suffix("restart the dev stack  →  docker compose up -d   [3xJ9kL]"),
            Some("3xJ9kL".to_string())
        );
    }

    #[test]
    fn returns_none_when_no_id_suffix_present() {
        assert_eq!(parse_id_suffix("just typed free text"), None);
    }
}
