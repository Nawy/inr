use crate::models::{CmdVarKind, CmdVariable, HistoryBinding};
use anyhow::{anyhow, Result};
use std::process::{Command, ExitStatus};
use zeroize::Zeroizing;

/// Where a resolved variable's value came from - kept only for history
/// logging (never the secret value itself, just the fact it came from an
/// env, and which one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingSource {
    Literal,
    Env(String),
}

pub struct ResolvedVar {
    pub var: CmdVariable,
    pub value: Zeroizing<String>,
    pub source: BindingSource,
}

impl ResolvedVar {
    /// Builds the history-safe record of this binding: a secret's actual
    /// value is never included, whether it was typed directly (hidden
    /// marker) or pulled from an env (only the env's name, as "@name").
    pub fn to_history_binding(&self) -> HistoryBinding {
        let display = match (&self.var.kind, &self.source) {
            (CmdVarKind::Secret, BindingSource::Literal) => "<entered directly, hidden>".to_string(),
            (_, BindingSource::Env(name)) => format!("@{name}"),
            (CmdVarKind::Text | CmdVarKind::Number, BindingSource::Literal) => self.value.to_string(),
        };
        HistoryBinding {
            name: self.var.name.clone(),
            kind: self.var.kind.as_str().to_string(),
            display,
        }
    }
}

/// Shell-quotes a literal value for POSIX shells (sh/bash/zsh/nu): wraps in
/// single quotes, escaping any embedded single quote. The result is always
/// treated as exactly one literal word, immune to expansion/injection.
pub fn quote_literal_posix(value: &str) -> String {
    shell_words::quote(value).into_owned()
}

/// Shell-quotes a literal value for PowerShell: single-quoted string with
/// embedded single quotes doubled (PowerShell's escaping rule).
pub fn quote_literal_powershell(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// A double-quoted reference to an environment variable that will be set on
/// the child process. Double quotes (not single) so the shell still expands
/// $VAR/$env:VAR, but the whole value stays one word even if it contains
/// spaces.
fn secret_ref_posix(env_var_name: &str) -> String {
    format!("\"${env_var_name}\"")
}

fn secret_ref_powershell(env_var_name: &str) -> String {
    format!("\"$env:{env_var_name}\"")
}

pub fn is_windows() -> bool {
    cfg!(target_os = "windows")
}

/// Builds the final, ready-to-run command line by substituting each
/// variable's placeholder in `template`. Text/number values are
/// shell-quoted literally; secret values are NEVER substituted as text -
/// instead an env-var reference is spliced in, and the actual secret is
/// returned separately to be set only on the child process's environment.
pub fn build_command_line(
    template: &str,
    resolved: &[ResolvedVar],
) -> Result<(String, Vec<(String, Zeroizing<String>)>)> {
    let mut command_line = template.to_string();
    let mut secret_envs = Vec::new();

    for (idx, rv) in resolved.iter().enumerate() {
        let placeholder = rv.var.placeholder();
        if !command_line.contains(&placeholder) {
            return Err(anyhow!("variable '{}' not found in template", rv.var.name));
        }
        let replacement = match rv.var.kind {
            CmdVarKind::Text | CmdVarKind::Number => {
                if is_windows() {
                    quote_literal_powershell(&rv.value)
                } else {
                    quote_literal_posix(&rv.value)
                }
            }
            CmdVarKind::Secret => {
                let env_name = format!("INR_SECRET_{}", idx + 1);
                let reference = if is_windows() {
                    secret_ref_powershell(&env_name)
                } else {
                    secret_ref_posix(&env_name)
                };
                secret_envs.push((env_name, rv.value.clone()));
                reference
            }
        };
        // Replace every occurrence: the same placeholder may appear more
        // than once in a template (dedup happens at parse_placeholders).
        command_line = command_line.replace(&placeholder, &replacement);
    }

    Ok((command_line, secret_envs))
}

/// Returns (program, base_args) for invoking the user's shell
/// non-interactively (no rc/profile sourcing).
pub fn shell_invocation() -> (String, Vec<String>) {
    if is_windows() {
        (
            "powershell.exe".to_string(),
            vec!["-NoProfile".to_string(), "-Command".to_string()],
        )
    } else {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        (shell, vec!["-c".to_string()])
    }
}

/// Runs `command_line` through the user's shell, inheriting stdio, with
/// `secret_envs` set only on the child process's environment (never in
/// argv, never in a shell history file).
pub fn run_inherited(
    command_line: &str,
    secret_envs: &[(String, Zeroizing<String>)],
) -> Result<ExitStatus> {
    let (program, base_args) = shell_invocation();
    let mut cmd = Command::new(program);
    cmd.args(base_args);
    cmd.arg(command_line);
    for (name, value) in secret_envs {
        cmd.env(name, value.as_str());
    }
    cmd.status()
        .map_err(|e| anyhow!("failed to run command: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::CmdVarKind;

    fn var(kind: CmdVarKind, name: &str) -> CmdVariable {
        CmdVariable {
            kind,
            name: name.to_string(),
        }
    }

    #[test]
    fn posix_quote_escapes_injection_attempt() {
        let quoted = quote_literal_posix("hi; rm -rf ~");
        assert_eq!(quoted, "'hi; rm -rf ~'");
    }

    #[test]
    fn posix_quote_escapes_embedded_single_quote() {
        let quoted = quote_literal_posix("it's a test");
        // shell_words escapes by closing/reopening the quote around '
        assert!(!quoted.contains("'s a test'a"));
        // round-trip through shell_words::split must recover the original
        let joined = format!("echo {quoted}");
        let parts = shell_words::split(&joined).unwrap();
        assert_eq!(parts, vec!["echo", "it's a test"]);
    }

    #[test]
    fn powershell_quote_escapes_embedded_single_quote() {
        assert_eq!(quote_literal_powershell("it's"), "'it''s'");
    }

    #[test]
    fn text_and_number_values_are_substituted_literally_and_quoted() {
        let resolved = vec![
            ResolvedVar {
                var: var(CmdVarKind::Text, "message"),
                value: Zeroizing::new("hello; whoami".to_string()),
                source: BindingSource::Literal,
            },
            ResolvedVar {
                var: var(CmdVarKind::Number, "count"),
                value: Zeroizing::new("42".to_string()),
                source: BindingSource::Literal,
            },
        ];
        let (line, secrets) =
            build_command_line("say %t:message repeated %n:count times", &resolved).unwrap();
        assert!(secrets.is_empty());
        // the injection attempt must be neutralized - kept inside a single
        // quoted token, not left as an unquoted, shell-interpretable value
        let quoted = if is_windows() {
            quote_literal_powershell("hello; whoami")
        } else {
            quote_literal_posix("hello; whoami")
        };
        assert!(line.contains(&quoted));
        assert!(line.contains("42"));
    }

    #[test]
    fn secret_value_never_appears_literally_in_command_line() {
        let resolved = vec![ResolvedVar {
            var: var(CmdVarKind::Secret, "token"),
            value: Zeroizing::new("sk-super-secret-value".to_string()),
            source: BindingSource::Literal,
        }];
        let (line, secrets) =
            build_command_line("curl -H 'Authorization: %s:token' api.example.com", &resolved)
                .unwrap();

        assert!(!line.contains("sk-super-secret-value"));
        assert_eq!(secrets.len(), 1);
        assert_eq!(secrets[0].0, "INR_SECRET_1");
        assert_eq!(secrets[0].1.as_str(), "sk-super-secret-value");
        // command line references the env var, doesn't inline the value
        assert!(line.contains("INR_SECRET_1"));
    }

    #[test]
    fn repeated_placeholder_is_replaced_everywhere() {
        let resolved = vec![ResolvedVar {
            var: var(CmdVarKind::Text, "msg"),
            value: Zeroizing::new("hi".to_string()),
            source: BindingSource::Literal,
        }];
        let (line, _) = build_command_line("echo %t:msg && echo %t:msg", &resolved).unwrap();
        assert_eq!(line.matches("hi").count(), 2);
        assert!(!line.contains("%t:msg"));
    }

    #[test]
    fn missing_placeholder_in_template_errors() {
        let resolved = vec![ResolvedVar {
            var: var(CmdVarKind::Text, "nope"),
            value: Zeroizing::new("x".to_string()),
            source: BindingSource::Literal,
        }];
        assert!(build_command_line("echo hi", &resolved).is_err());
    }
}
