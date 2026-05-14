use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

pub const DEFAULT_BOARD_SERVER_IP: &str = "localhost";
pub const DEFAULT_BOARD_SERVER_PORT: u16 = 2999;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct BoardGlobalConfigFile {
    #[serde(default)]
    pub board: BoardGlobalConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BoardGlobalConfig {
    #[serde(default = "default_server_ip")]
    pub server_ip: String,
    #[serde(default = "default_server_port")]
    pub port: u16,
}

impl Default for BoardGlobalConfig {
    fn default() -> Self {
        Self {
            server_ip: DEFAULT_BOARD_SERVER_IP.to_string(),
            port: DEFAULT_BOARD_SERVER_PORT,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LoadedBoardGlobalConfig {
    pub path: PathBuf,
    pub board: BoardGlobalConfig,
    pub created: bool,
}

impl LoadedBoardGlobalConfig {
    pub fn load_or_create() -> anyhow::Result<Self> {
        let path = default_config_path()?;
        Self::load_or_create_at(&path)
    }

    pub fn load_or_create_at(path: &Path) -> anyhow::Result<Self> {
        match fs::read_to_string(path) {
            Ok(content) => {
                let file: BoardGlobalConfigFile = toml::from_str(&content)
                    .with_context(|| format!("failed to parse {}", path.display()))?;
                file.board.validate(path)?;
                Ok(Self {
                    path: path.to_path_buf(),
                    board: file.board,
                    created: false,
                })
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let file = BoardGlobalConfigFile::default();
                write_config_file(path, &file)?;
                Ok(Self {
                    path: path.to_path_buf(),
                    board: file.board,
                    created: true,
                })
            }
            Err(err) => Err(err).with_context(|| format!("failed to read {}", path.display())),
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        write_config_file(
            &self.path,
            &BoardGlobalConfigFile {
                board: self.board.clone(),
            },
        )
    }

    pub fn resolve_server(&self, cli_server: Option<&str>, cli_port: Option<u16>) -> (String, u16) {
        self.board.resolve_server(cli_server, cli_port)
    }
}

impl BoardGlobalConfig {
    pub fn resolve_server(&self, cli_server: Option<&str>, cli_port: Option<u16>) -> (String, u16) {
        let server_ip = cli_server
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| self.server_ip.clone());
        let port = cli_port.unwrap_or(self.port);
        (server_ip, port)
    }

    pub fn validate(&self, path: &Path) -> anyhow::Result<()> {
        if self.server_ip.trim().is_empty() {
            bail!("`board.server_ip` must not be empty in {}", path.display());
        }
        if self.port == 0 {
            bail!("`board.port` must be in 1..=65535 in {}", path.display());
        }
        Ok(())
    }
}

fn default_server_ip() -> String {
    DEFAULT_BOARD_SERVER_IP.to_string()
}

const fn default_server_port() -> u16 {
    DEFAULT_BOARD_SERVER_PORT
}

fn default_config_path() -> anyhow::Result<PathBuf> {
    let home = env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".ostool").join("config.toml"))
}

fn write_config_file(path: &Path, file: &BoardGlobalConfigFile) -> anyhow::Result<()> {
    file.board.validate(path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(path, toml::to_string_pretty(file)?)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{BoardGlobalConfig, LoadedBoardGlobalConfig};

    #[test]
    fn load_or_create_creates_default_config_when_missing() {
        let temp = tempdir().unwrap();
        let path = temp.path().join(".ostool/config.toml");

        let loaded = LoadedBoardGlobalConfig::load_or_create_at(&path).unwrap();

        assert!(loaded.created);
        assert_eq!(loaded.board.server_ip, "localhost");
        assert_eq!(loaded.board.port, 2999);
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("[board]"));
        assert!(content.contains("server_ip = \"localhost\""));
        assert!(content.contains("port = 2999"));
    }

    #[test]
    fn resolve_server_prefers_cli_over_global_defaults() {
        let config = BoardGlobalConfig {
            server_ip: "10.0.0.2".into(),
            port: 8000,
        };

        assert_eq!(
            config.resolve_server(Some("192.168.1.2"), Some(9000)),
            ("192.168.1.2".to_string(), 9000)
        );
        assert_eq!(
            config.resolve_server(None, None),
            ("10.0.0.2".to_string(), 8000)
        );
    }

    #[test]
    fn save_persists_updated_values() {
        let temp = tempdir().unwrap();
        let path = temp.path().join(".ostool/config.toml");
        let mut loaded = LoadedBoardGlobalConfig::load_or_create_at(&path).unwrap();

        loaded.board.server_ip = "10.0.0.2".into();
        loaded.board.port = 9000;
        loaded.save().unwrap();

        let reloaded = LoadedBoardGlobalConfig::load_or_create_at(&path).unwrap();
        assert!(!reloaded.created);
        assert_eq!(reloaded.board.server_ip, "10.0.0.2");
        assert_eq!(reloaded.board.port, 9000);
    }
}
