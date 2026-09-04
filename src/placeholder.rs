use crate::models::{CmdVarKind, CmdVariable};
use anyhow::{anyhow, Result};
use regex::Regex;
use std::sync::LazyLock;

static PLACEHOLDER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"%([stn]):([A-Za-z_][A-Za-z0-9_]*)").unwrap());

/// Extracts `%s:name`/`%t:name`/`%n:name` placeholders from a command
/// template, in order of first appearance, deduplicated by name. Returns an
/// error if the same name is used with two different kinds.
pub fn parse_placeholders(template: &str) -> Result<Vec<CmdVariable>> {
    let mut vars: Vec<CmdVariable> = Vec::new();
    for caps in PLACEHOLDER_RE.captures_iter(template) {
        let kind = match &caps[1] {
            "s" => CmdVarKind::Secret,
            "t" => CmdVarKind::Text,
            "n" => CmdVarKind::Number,
            _ => unreachable!("regex only matches s|t|n"),
        };
        let name = caps[2].to_string();

        if let Some(existing) = vars.iter().find(|v| v.name == name) {
            if existing.kind != kind {
                return Err(anyhow!(
                    "variable '{name}' used with conflicting kinds ({} and {})",
                    existing.kind,
                    kind
                ));
            }
            continue;
        }
        vars.push(CmdVariable { kind, name });
    }
    Ok(vars)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_placeholders_from_template() {
        let vars = parse_placeholders("ssh %s:host -p %n:port -m %t:message").unwrap();
        assert_eq!(
            vars,
            vec![
                CmdVariable {
                    kind: CmdVarKind::Secret,
                    name: "host".into()
                },
                CmdVariable {
                    kind: CmdVarKind::Number,
                    name: "port".into()
                },
                CmdVariable {
                    kind: CmdVarKind::Text,
                    name: "message".into()
                },
            ]
        );
    }

    #[test]
    fn no_placeholders_returns_empty() {
        assert!(parse_placeholders("docker compose up -d").unwrap().is_empty());
    }

    #[test]
    fn dedups_repeated_same_kind_placeholder() {
        let vars = parse_placeholders("echo %t:msg && echo %t:msg again").unwrap();
        assert_eq!(vars.len(), 1);
    }

    #[test]
    fn conflicting_kind_for_same_name_errors() {
        let err = parse_placeholders("echo %t:x && echo %s:x").unwrap_err();
        assert!(err.to_string().contains("conflicting kinds"));
    }
}
