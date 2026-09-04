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

/// A command variable as actually stored in the DB: name/kind plus its
/// optional default env. `default_env_id` is an unconstrained id (no FK) -
/// the env it points to may have been deleted since; that's resolved
/// lazily by whoever reads it (`vars::resolve_variable`), never tracked or
/// cascaded here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCommandVariable {
    pub var: CmdVariable,
    pub default_env_id: Option<String>,
}

/// Result of comparing a command's previously-stored variables against a
/// freshly re-parsed set from an edited template, matched by (name, kind).
/// Used by `inr e` to summarize what will change before saving - a
/// variable whose name is reused with a different kind counts as one
/// removal plus one addition, never a "kept".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariableDiff {
    pub kept: Vec<CmdVariable>,
    pub added: Vec<CmdVariable>,
    pub removed: Vec<StoredCommandVariable>,
}

impl VariableDiff {
    pub fn is_unchanged(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

pub fn diff_variables(old: &[StoredCommandVariable], new: &[CmdVariable]) -> VariableDiff {
    let matches = |o: &StoredCommandVariable, n: &CmdVariable| o.var.name == n.name && o.var.kind == n.kind;

    let kept = new.iter().filter(|n| old.iter().any(|o| matches(o, n))).cloned().collect();
    let added = new.iter().filter(|n| !old.iter().any(|o| matches(o, n))).cloned().collect();
    let removed = old.iter().filter(|o| !new.iter().any(|n| matches(o, n))).cloned().collect();

    VariableDiff { kept, added, removed }
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

    fn cmdvar(kind: CmdVarKind, name: &str) -> CmdVariable {
        CmdVariable { kind, name: name.to_string() }
    }

    fn stored(kind: CmdVarKind, name: &str, default_env_id: Option<&str>) -> StoredCommandVariable {
        StoredCommandVariable {
            var: cmdvar(kind, name),
            default_env_id: default_env_id.map(str::to_string),
        }
    }

    #[test]
    fn diff_variables_classifies_kept_added_removed() {
        let old = vec![
            stored(CmdVarKind::Text, "host", Some("env1")),
            stored(CmdVarKind::Secret, "token", None),
        ];
        let new = vec![
            cmdvar(CmdVarKind::Text, "host"),  // kept (same name+kind)
            cmdvar(CmdVarKind::Number, "port"), // added
            // "token" is gone -> removed
        ];

        let diff = diff_variables(&old, &new);
        assert_eq!(diff.kept, vec![cmdvar(CmdVarKind::Text, "host")]);
        assert_eq!(diff.added, vec![cmdvar(CmdVarKind::Number, "port")]);
        assert_eq!(diff.removed, vec![stored(CmdVarKind::Secret, "token", None)]);
        assert!(!diff.is_unchanged());
    }

    #[test]
    fn diff_variables_treats_kind_change_as_remove_plus_add() {
        let old = vec![stored(CmdVarKind::Text, "count", Some("env1"))];
        let new = vec![cmdvar(CmdVarKind::Number, "count")];

        let diff = diff_variables(&old, &new);
        assert!(diff.kept.is_empty());
        assert_eq!(diff.added, vec![cmdvar(CmdVarKind::Number, "count")]);
        assert_eq!(diff.removed, vec![stored(CmdVarKind::Text, "count", Some("env1"))]);
    }

    #[test]
    fn diff_variables_identical_sets_is_unchanged() {
        let old = vec![stored(CmdVarKind::Text, "host", None)];
        let new = vec![cmdvar(CmdVarKind::Text, "host")];

        let diff = diff_variables(&old, &new);
        assert!(diff.is_unchanged());
        assert_eq!(diff.kept, vec![cmdvar(CmdVarKind::Text, "host")]);
    }
}
