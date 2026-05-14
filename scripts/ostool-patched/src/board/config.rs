use std::{env::current_dir, path::PathBuf};

use anyhow::Context as _;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    Tool, board::global_config::BoardGlobalConfig, run::shell_init::normalize_shell_init_config,
};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default, PartialEq, Eq)]
pub struct BoardRunConfig {
    pub board_type: String,
    pub dtb_file: Option<String>,
    #[serde(default)]
    pub success_regex: Vec<String>,
    #[serde(default)]
    pub fail_regex: Vec<String>,
    #[serde(default)]
    pub uboot_cmd: Option<Vec<String>>,
    pub shell_prefix: Option<String>,
    pub shell_init_cmd: Option<String>,
    pub timeout: Option<u64>,
    pub server: Option<String>,
    pub port: Option<u16>,
}

impl BoardRunConfig {
    pub(crate) fn default_path(explicit_path: Option<PathBuf>) -> anyhow::Result<PathBuf> {
        match explicit_path {
            Some(path) => Ok(path),
            None => Ok(current_dir()?.join(".board.toml")),
        }
    }

    pub(crate) async fn load_or_create(
        tool: &Tool,
        explicit_path: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let config_path = Self::default_path(explicit_path)?;
        let mut config = jkconfig::run::<Self>(config_path.clone(), false, &[])
            .await
            .with_context(|| format!("failed to load board config: {}", config_path.display()))?
            .ok_or_else(|| anyhow!("No board configuration obtained"))?;
        config.replace_strings(tool)?;
        config.normalize(&format!("board config {}", config_path.display()))?;
        Ok(config)
    }

    pub(crate) fn read_from_path(tool: &Tool, path: PathBuf) -> anyhow::Result<Self> {
        let mut config: Self = toml::from_str(
            &std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read board config: {}", path.display()))?,
        )
        .with_context(|| format!("failed to parse board config: {}", path.display()))?;
        config.replace_strings(tool)?;
        config.normalize(&format!("board config {}", path.display()))?;
        Ok(config)
    }

    pub(crate) fn resolve_server(
        &self,
        cli_server: Option<&str>,
        cli_port: Option<u16>,
        global_config: &BoardGlobalConfig,
    ) -> (String, u16) {
        let server = cli_server
            .map(str::to_string)
            .or_else(|| self.server.clone())
            .unwrap_or_else(|| global_config.server_ip.clone());
        let port = cli_port.or(self.port).unwrap_or(global_config.port);
        (server, port)
    }

    pub(crate) fn apply_overrides(
        &mut self,
        tool: &Tool,
        board_type: Option<&str>,
        server: Option<&str>,
        port: Option<u16>,
    ) -> anyhow::Result<()> {
        if let Some(board_type) = board_type {
            self.board_type = tool.replace_string(board_type)?;
        }

        if let Some(server) = server {
            let server = tool.replace_string(server)?;
            let server = server.trim().to_string();
            if server.is_empty() {
                anyhow::bail!("board server override must not be empty");
            }
            self.server = Some(server);
        }

        if let Some(port) = port {
            if port == 0 {
                anyhow::bail!("board port override must be in 1..=65535");
            }
            self.port = Some(port);
        }

        self.normalize("board run arguments")
    }

    fn replace_strings(&mut self, tool: &Tool) -> anyhow::Result<()> {
        self.board_type = tool.replace_string(&self.board_type)?;
        self.dtb_file = self
            .dtb_file
            .as_deref()
            .map(|value| tool.replace_string(value))
            .transpose()?;
        self.success_regex = self
            .success_regex
            .iter()
            .map(|value| tool.replace_string(value))
            .collect::<anyhow::Result<Vec<_>>>()?;
        self.fail_regex = self
            .fail_regex
            .iter()
            .map(|value| tool.replace_string(value))
            .collect::<anyhow::Result<Vec<_>>>()?;
        self.uboot_cmd = self
            .uboot_cmd
            .as_ref()
            .map(|values| {
                values
                    .iter()
                    .map(|value| tool.replace_string(value))
                    .collect::<anyhow::Result<Vec<_>>>()
            })
            .transpose()?;
        self.shell_prefix = self
            .shell_prefix
            .as_deref()
            .map(|value| tool.replace_string(value))
            .transpose()?;
        self.shell_init_cmd = self
            .shell_init_cmd
            .as_deref()
            .map(|value| tool.replace_string(value))
            .transpose()?;
        self.server = self
            .server
            .as_deref()
            .map(|value| tool.replace_string(value))
            .transpose()?;
        Ok(())
    }

    fn normalize(&mut self, config_name: &str) -> anyhow::Result<()> {
        self.board_type = self.board_type.trim().to_string();
        if let Some(dtb_file) = self.dtb_file.as_mut() {
            let trimmed = dtb_file.trim();
            if trimmed.is_empty() {
                self.dtb_file = None;
            } else if trimmed.len() != dtb_file.len() {
                *dtb_file = trimmed.to_string();
            }
        }
        if let Some(commands) = self.uboot_cmd.as_mut() {
            commands.retain_mut(|command| {
                let trimmed = command.trim();
                if trimmed.is_empty() {
                    false
                } else {
                    if trimmed.len() != command.len() {
                        *command = trimmed.to_string();
                    }
                    true
                }
            });
            if commands.is_empty() {
                self.uboot_cmd = None;
            }
        }
        if self.board_type.is_empty() {
            anyhow::bail!("`board_type` must not be empty in {config_name}");
        }
        normalize_shell_init_config(
            &mut self.shell_prefix,
            &mut self.shell_init_cmd,
            config_name,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::BoardRunConfig;
    use crate::{
        Tool, ToolConfig,
        board::global_config::BoardGlobalConfig,
        build::config::{BuildConfig, BuildSystem, Cargo},
    };
    use std::collections::HashMap;

    #[test]
    fn board_run_config_parses_and_normalizes_shell_fields() {
        let mut config: BoardRunConfig = toml::from_str(
            r#"
board_type = " orangepi5plus "
dtb_file = " ${workspace}/board.dtb "
success_regex = ["ok"]
fail_regex = ["panic"]
uboot_cmd = [" run bootcmd "]
shell_prefix = " login: "
shell_init_cmd = " root "
timeout = 15
server = "10.0.0.2"
port = 9000
"#,
        )
        .unwrap();

        config.normalize("test board config").unwrap();

        assert_eq!(config.board_type, "orangepi5plus");
        assert_eq!(config.dtb_file.as_deref(), Some("${workspace}/board.dtb"));
        assert_eq!(config.uboot_cmd, Some(vec!["run bootcmd".to_string()]));
        assert_eq!(config.shell_prefix.as_deref(), Some("login:"));
        assert_eq!(config.shell_init_cmd.as_deref(), Some("root"));
        assert_eq!(config.timeout, Some(15));
        assert_eq!(
            config.resolve_server(
                Some("127.0.0.1"),
                None,
                &BoardGlobalConfig {
                    server_ip: "localhost".into(),
                    port: 2999,
                }
            ),
            ("127.0.0.1".to_string(), 9000)
        );
    }

    #[test]
    fn board_run_config_default_path_uses_current_dir() {
        let path = BoardRunConfig::default_path(None).unwrap();
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some(".board.toml")
        );
    }

    #[test]
    fn board_run_config_apply_overrides_replaces_board_type_and_server() {
        let mut config: BoardRunConfig = toml::from_str(
            r#"
board_type = "orangepi5plus"
server = "10.0.0.2"
port = 9000
"#,
        )
        .unwrap();
        let tool = Tool::new(Default::default()).unwrap();

        config
            .apply_overrides(&tool, Some(" rk3568 "), Some(" 127.0.0.1 "), Some(7000))
            .unwrap();

        assert_eq!(config.board_type, "rk3568");
        assert_eq!(config.server.as_deref(), Some("127.0.0.1"));
        assert_eq!(config.port, Some(7000));
    }

    #[tokio::test]
    async fn read_board_run_config_from_path_normalizes_loaded_values() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"sample\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/lib.rs"), "").unwrap();
        let config_path = tmp.path().join("custom.board.toml");
        std::fs::write(
            &config_path,
            r#"
board_type = " rk3568 "
shell_prefix = " login: "
shell_init_cmd = " root "
timeout = 8
"#,
        )
        .unwrap();

        let mut tool = Tool::new(ToolConfig {
            manifest: Some(tmp.path().to_path_buf()),
            ..Default::default()
        })
        .unwrap();

        let config = tool
            .read_board_run_config_from_path(&config_path)
            .await
            .unwrap();
        assert_eq!(config.board_type, "rk3568");
        assert_eq!(config.shell_prefix.as_deref(), Some("login:"));
        assert_eq!(config.shell_init_cmd.as_deref(), Some("root"));
        assert_eq!(config.timeout, Some(8));
    }

    #[tokio::test]
    async fn ensure_board_run_config_in_dir_replaces_package_variables() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\", \"kernel\"]\nresolver = \"3\"\n",
        )
        .unwrap();

        let app_dir = tmp.path().join("app");
        std::fs::create_dir_all(app_dir.join("src")).unwrap();
        std::fs::write(
            app_dir.join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::write(app_dir.join("src/main.rs"), "fn main() {}\n").unwrap();

        let kernel_dir = tmp.path().join("kernel");
        std::fs::create_dir_all(kernel_dir.join("src")).unwrap();
        std::fs::write(
            kernel_dir.join("Cargo.toml"),
            "[package]\nname = \"kernel\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::write(kernel_dir.join("src/main.rs"), "fn main() {}\n").unwrap();

        std::fs::write(
            tmp.path().join(".board.toml"),
            r#"
board_type = "kernel-board"
dtb_file = "${package}/board.dtb"
"#,
        )
        .unwrap();

        let mut tool = Tool::new(ToolConfig {
            manifest: Some(app_dir),
            ..Default::default()
        })
        .unwrap();
        tool.ctx.build_config = Some(BuildConfig {
            system: BuildSystem::Cargo(Cargo {
                env: HashMap::new(),
                target: "aarch64-unknown-none".into(),
                package: "kernel".into(),
                features: vec![],
                log: None,
                extra_config: None,
                args: vec![],
                pre_build_cmds: vec![],
                post_build_cmds: vec![],
                to_bin: false,
            }),
        });

        let config = tool
            .ensure_board_run_config_in_dir(tmp.path())
            .await
            .unwrap();
        let expected = kernel_dir.join("board.dtb").display().to_string();
        assert_eq!(config.dtb_file.as_deref(), Some(expected.as_str()));
    }
}
