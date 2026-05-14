use std::time::Duration;

use anyhow::{Result, bail};

pub(crate) const SHELL_INIT_DELAY: Duration = Duration::from_millis(100);

pub(crate) fn normalize_shell_init_config(
    shell_prefix: &mut Option<String>,
    shell_init_cmd: &mut Option<String>,
    config_name: &str,
) -> Result<()> {
    normalize_optional_field(shell_prefix);
    normalize_optional_field(shell_init_cmd);

    if shell_init_cmd.is_some() && shell_prefix.is_none() {
        bail!("`shell_init_cmd` requires `shell_prefix` in {config_name}");
    }

    Ok(())
}

fn normalize_optional_field(value: &mut Option<String>) {
    if let Some(raw) = value {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            *value = None;
        } else if trimmed.len() != raw.len() {
            *raw = trimmed.to_string();
        }
    }
}

pub(crate) fn prepare_shell_init_cmd(command: &str) -> Vec<u8> {
    let mut normalized = command.trim_end_matches(['\r', '\n']).as_bytes().to_vec();
    normalized.push(b'\n');
    normalized
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShellAutoInitMatcher {
    shell_prefix: String,
    shell_init_cmd: Vec<u8>,
    history: Vec<u8>,
    triggered: bool,
}

impl ShellAutoInitMatcher {
    pub(crate) fn new(
        shell_prefix: Option<String>,
        shell_init_cmd: Option<String>,
    ) -> Option<Self> {
        match (shell_prefix, shell_init_cmd) {
            (Some(shell_prefix), Some(shell_init_cmd)) => Some(Self {
                history: Vec::with_capacity(shell_prefix.len().max(64)),
                shell_prefix,
                shell_init_cmd: prepare_shell_init_cmd(&shell_init_cmd),
                triggered: false,
            }),
            _ => None,
        }
    }

    pub(crate) fn observe_byte(&mut self, byte: u8) -> Option<Vec<u8>> {
        if self.triggered {
            return None;
        }

        self.history.push(byte);
        self.trim_history();

        if String::from_utf8_lossy(&self.history).contains(&self.shell_prefix) {
            self.triggered = true;
            return Some(self.shell_init_cmd.clone());
        }

        None
    }

    fn trim_history(&mut self) {
        let max_len = self.shell_prefix.len().max(64) * 8;
        if self.history.len() > max_len {
            let excess = self.history.len() - max_len;
            self.history.drain(..excess);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ShellAutoInitMatcher, normalize_shell_init_config, prepare_shell_init_cmd};

    #[test]
    fn normalize_shell_init_config_rejects_missing_prefix() {
        let mut shell_prefix = None;
        let mut shell_init_cmd = Some("echo ready".to_string());

        let err =
            normalize_shell_init_config(&mut shell_prefix, &mut shell_init_cmd, "QEMU config")
                .unwrap_err();

        assert!(err.to_string().contains("shell_prefix"));
    }

    #[test]
    fn normalize_shell_init_config_trims_fields() {
        let mut shell_prefix = Some("  login: ".to_string());
        let mut shell_init_cmd = Some("  root  ".to_string());

        normalize_shell_init_config(&mut shell_prefix, &mut shell_init_cmd, "QEMU config").unwrap();

        assert_eq!(shell_prefix.as_deref(), Some("login:"));
        assert_eq!(shell_init_cmd.as_deref(), Some("root"));
    }

    #[test]
    fn prepare_shell_init_cmd_appends_single_newline() {
        assert_eq!(prepare_shell_init_cmd("root"), b"root\n");
        assert_eq!(prepare_shell_init_cmd("root\n"), b"root\n");
        assert_eq!(prepare_shell_init_cmd("root\r\n"), b"root\n");
    }

    #[test]
    fn shell_auto_init_matcher_triggers_once() {
        let mut matcher =
            ShellAutoInitMatcher::new(Some("login:".to_string()), Some("root".to_string()))
                .unwrap();

        let mut matched = None;
        for byte in b"noise login: login:" {
            if let Some(command) = matcher.observe_byte(*byte) {
                matched = Some(command);
            }
        }

        assert_eq!(matched.as_deref(), Some(&b"root\n"[..]));
        assert_eq!(matcher.observe_byte(b':'), None);
    }
}
