use crate::models::{CmdVarKind, CmdVariable};
use anyhow::{anyhow, Result};
use regex::Regex;
use std::sync::LazyLock;

static PLACEHOLDER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[%([stn]):([A-Za-z_][A-Za-z0-9_]*)\]|%([stn]):([A-Za-z_][A-Za-z0-9_]*)").unwrap()
});

fn kind_from_letter(letter: &str) -> CmdVarKind {
    match letter {
        "s" => CmdVarKind::Secret,
        "t" => CmdVarKind::Text,
        "n" => CmdVarKind::Number,
        _ => unreachable!("regex only matches s|t|n"),
    }
}

/// Extracts `%s:name`/`%t:name`/`%n:name` placeholders from a command
/// template, in order of first appearance, deduplicated by name. A
/// placeholder may also be wrapped in brackets, `[%t:name]`/`[%n:name]`, to
/// terminate its name before adjacent literal text that would otherwise be
/// swallowed into it (e.g. `[%n:count]ms` vs. bare `%n:countms`). Secret
/// placeholders may never use the bracketed form - their value must never
/// be inlined as literal text (see `shell::build_command_line`), so
/// `[%s:name]` is rejected immediately rather than silently accepted.
/// Returns an error if the same name is used with two different kinds.
pub fn parse_placeholders(template: &str) -> Result<Vec<CmdVariable>> {
    let mut vars: Vec<CmdVariable> = Vec::new();
    for caps in PLACEHOLDER_RE.captures_iter(template) {
        let (letter, name) = match (caps.get(1), caps.get(2)) {
            (Some(letter), Some(name)) => {
                if letter.as_str() == "s" {
                    return Err(anyhow!(
                        "secret variable '{n}' cannot use bracket syntax [%s:{n}] - secrets are never inlined as literal text; use bare %s:{n} instead",
                        n = name.as_str()
                    ));
                }
                (letter.as_str(), name.as_str())
            }
            _ => (
                caps.get(3).expect("bare kind group matched").as_str(),
                caps.get(4).expect("bare name group matched").as_str(),
            ),
        };
        let kind = kind_from_letter(letter);
        let name = name.to_string();

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

    #[test]
    fn parses_bracketed_text_and_number_placeholders() {
        let vars = parse_placeholders("[%n:number]ether and [%t:name]s").unwrap();
        assert_eq!(
            vars,
            vec![
                CmdVariable {
                    kind: CmdVarKind::Number,
                    name: "number".into()
                },
                CmdVariable {
                    kind: CmdVarKind::Text,
                    name: "name".into()
                },
            ]
        );
    }

    #[test]
    fn bare_and_bracketed_forms_of_same_variable_dedup_together() {
        let vars = parse_placeholders("echo %t:msg and [%t:msg]!").unwrap();
        assert_eq!(vars, vec![CmdVariable { kind: CmdVarKind::Text, name: "msg".into() }]);
    }

    #[test]
    fn bracketed_secret_is_rejected_immediately() {
        let err = parse_placeholders("curl -H 'Authorization: [%s:token]'").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("secret"));
        assert!(msg.contains("[%s:token]"));
    }

    #[test]
    fn bare_secret_is_still_allowed() {
        let vars = parse_placeholders("curl -H 'Authorization: %s:token'").unwrap();
        assert_eq!(vars, vec![CmdVariable { kind: CmdVarKind::Secret, name: "token".into() }]);
    }

    #[test]
    fn bare_form_without_brackets_greedily_consumes_adjacent_text() {
        // Documented, not fixed: without brackets, adjacent identifier
        // characters are swallowed into the name - `numberether` is parsed
        // as one variable, not `number` + literal `ether`.
        let vars = parse_placeholders("[%n:number]ether and %n:numberether").unwrap();
        assert_eq!(
            vars,
            vec![
                CmdVariable { kind: CmdVarKind::Number, name: "number".into() },
                CmdVariable { kind: CmdVarKind::Number, name: "numberether".into() },
            ]
        );
    }

    #[test]
    fn unambiguous_bare_placeholder_needs_no_brackets() {
        let vars = parse_placeholders("ssh %s:host -p %n:port -m %t:message").unwrap();
        assert_eq!(vars.len(), 3);
    }
}
