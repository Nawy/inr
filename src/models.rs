use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Kind of a `%s:`/`%t:`/`%n:` placeholder inside a stored command template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdVarKind {
    Secret,
    Text,
    Number,
}

impl CmdVarKind {
    pub fn as_str(self) -> &'static str {
        match self {
            CmdVarKind::Secret => "secret",
            CmdVarKind::Text => "text",
            CmdVarKind::Number => "number",
        }
    }

    pub fn from_str(s: &str) -> Result<Self> {
        match s {
            "secret" => Ok(CmdVarKind::Secret),
            "text" => Ok(CmdVarKind::Text),
            "number" => Ok(CmdVarKind::Number),
            other => Err(anyhow!("unknown command variable kind: {other}")),
        }
    }

    pub fn placeholder_prefix(self) -> &'static str {
        match self {
            CmdVarKind::Secret => "%s:",
            CmdVarKind::Text => "%t:",
            CmdVarKind::Number => "%n:",
        }
    }
}

impl fmt::Display for CmdVarKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// A variable definition parsed out of a command template (name + kind only,
/// no value - values are entered fresh each time the command is run).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdVariable {
    pub kind: CmdVarKind,
    pub name: String,
}

impl CmdVariable {
    pub fn placeholder(&self) -> String {
        format!("{}{}", self.kind.placeholder_prefix(), self.name)
    }
}

#[derive(Debug, Clone)]
pub struct StoredCommand {
    pub id: String,
    pub template: String,
    pub description: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Kind of an `inr env` variable. Distinct from `CmdVarKind`: env variables
/// distinguish float vs int, command placeholders only have one generic
/// number kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvKind {
    Secret,
    Text,
    NumberFloat,
    NumberInt,
}

impl EnvKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EnvKind::Secret => "secret",
            EnvKind::Text => "text",
            EnvKind::NumberFloat => "number_float",
            EnvKind::NumberInt => "number_int",
        }
    }

    pub fn from_str(s: &str) -> Result<Self> {
        match s {
            "secret" => Ok(EnvKind::Secret),
            "text" => Ok(EnvKind::Text),
            "number_float" => Ok(EnvKind::NumberFloat),
            "number_int" => Ok(EnvKind::NumberInt),
            other => Err(anyhow!("unknown env kind: {other}")),
        }
    }

    /// Whether an env of this kind may be bound into a command variable of
    /// `cmd_kind` via `@name` lookup. A secret env may only fill a %s:
    /// variable - never %t:/%n: - or its value would leak into argv/history.
    pub fn compatible_with(self, cmd_kind: CmdVarKind) -> bool {
        match (self, cmd_kind) {
            (EnvKind::Secret, CmdVarKind::Secret) => true,
            (EnvKind::Text, CmdVarKind::Text) => true,
            (EnvKind::NumberFloat, CmdVarKind::Number) => true,
            (EnvKind::NumberInt, CmdVarKind::Number) => true,
            _ => false,
        }
    }
}

impl fmt::Display for EnvKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Parses an `inr env a <spec>` name argument into (kind, bare name).
/// Recognized prefixes: `s:`, `t:`, `nf:`, `ni:`. No prefix defaults to Text.
pub fn parse_env_spec(spec: &str) -> (EnvKind, String) {
    for (prefix, kind) in [
        ("s:", EnvKind::Secret),
        ("t:", EnvKind::Text),
        ("nf:", EnvKind::NumberFloat),
        ("ni:", EnvKind::NumberInt),
    ] {
        if let Some(rest) = spec.strip_prefix(prefix) {
            return (kind, rest.to_string());
        }
    }
    (EnvKind::Text, spec.to_string())
}

#[derive(Debug, Clone)]
pub struct StoredEnv {
    pub id: String,
    pub name: String,
    pub kind: EnvKind,
    /// Plaintext for Text/NumberFloat/NumberInt, ciphertext for Secret.
    pub value: Vec<u8>,
    /// Only present (Some) for Secret kind.
    pub nonce: Option<Vec<u8>>,
    pub description: String,
    pub created_at: String,
    pub updated_at: String,
}

/// A single variable's record within one history entry. `display` is
/// either the literal typed value (text/number), "@envname" (value pulled
/// from an env, any kind), or a hidden marker for a secret typed directly -
/// the actual secret value is never recorded here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryBinding {
    pub name: String,
    pub kind: String,
    pub display: String,
}

#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub id: i64,
    pub command_id: String,
    pub template: String,
    pub executed_at: String,
    pub bindings: Vec<HistoryBinding>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_env_spec_prefixes() {
        assert_eq!(
            parse_env_spec("s:apikey"),
            (EnvKind::Secret, "apikey".to_string())
        );
        assert_eq!(
            parse_env_spec("t:username"),
            (EnvKind::Text, "username".to_string())
        );
        assert_eq!(
            parse_env_spec("nf:threshold"),
            (EnvKind::NumberFloat, "threshold".to_string())
        );
        assert_eq!(
            parse_env_spec("ni:count"),
            (EnvKind::NumberInt, "count".to_string())
        );
        assert_eq!(
            parse_env_spec("plainname"),
            (EnvKind::Text, "plainname".to_string())
        );
    }

    #[test]
    fn env_kind_compatibility_blocks_secret_leak() {
        assert!(EnvKind::Secret.compatible_with(CmdVarKind::Secret));
        assert!(!EnvKind::Secret.compatible_with(CmdVarKind::Text));
        assert!(!EnvKind::Secret.compatible_with(CmdVarKind::Number));
        assert!(EnvKind::NumberFloat.compatible_with(CmdVarKind::Number));
        assert!(EnvKind::NumberInt.compatible_with(CmdVarKind::Number));
        assert!(!EnvKind::Text.compatible_with(CmdVarKind::Number));
    }
}
