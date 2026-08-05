use std::{
    path::{Component, Path, PathBuf},
    str::FromStr,
};

use anyhow::Context;
use clap::{Args, Subcommand};
use ostool::board;

#[derive(Subcommand, Debug)]
pub enum Command {
    /// List available remote board types
    Ls(BoardServerArgs),
    /// Allocate a remote board and connect to its serial terminal
    Connect(ArgsConnect),
    /// Edit the default board server configuration
    Config,
}

#[derive(Args, Debug, Default, Clone)]
pub struct BoardServerArgs {
    /// ostool-server host
    #[arg(long)]
    pub server: Option<String>,
    /// ostool-server port
    #[arg(long)]
    pub port: Option<u16>,
}

#[derive(Args, Debug, Clone)]
pub struct ArgsConnect {
    /// Board type to allocate and connect
    #[arg(short = 'b', long)]
    pub board_type: String,

    /// Upload RELATIVE_PATH=LOCAL_PATH and print its board-visible HTTP URL
    #[arg(long = "session-file", value_name = "RELATIVE_PATH=LOCAL_PATH")]
    pub session_files: Vec<SessionFileArg>,

    #[command(flatten)]
    pub server: BoardServerArgs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFileArg {
    relative_path: String,
    local_path: PathBuf,
}

impl SessionFileArg {
    pub(crate) fn new(relative_path: String, local_path: PathBuf) -> Result<Self, String> {
        if !is_safe_session_relative_path(&relative_path) {
            return Err(
                "session relative path must contain only normal path components".to_string(),
            );
        }
        if local_path.as_os_str().is_empty() {
            return Err("local path must not be empty".to_string());
        }
        Ok(Self {
            relative_path,
            local_path,
        })
    }
}

impl FromStr for SessionFileArg {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (relative_path, local_path) = value
            .split_once('=')
            .ok_or_else(|| "expected RELATIVE_PATH=LOCAL_PATH".to_string())?;
        Self::new(relative_path.to_string(), PathBuf::from(local_path))
    }
}

fn is_safe_session_relative_path(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

pub async fn execute(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Ls(server) => {
            let global_config = board::load_board_global_config_with_notice()?;
            let endpoint = global_config.resolve_endpoint(server.server.as_deref(), server.port)?;
            let boards = board::fetch_board_types_endpoint(endpoint).await?;
            println!("{}", board::render_board_table(&boards));
            Ok(())
        }
        Command::Connect(args) => {
            let global_config = board::load_board_global_config_with_notice()?;
            let endpoint =
                global_config.resolve_endpoint(args.server.server.as_deref(), args.server.port)?;
            if args.session_files.is_empty() {
                board::connect_board_endpoint(endpoint, &args.board_type).await
            } else {
                connect_with_session_files(endpoint, &args.board_type, &args.session_files).await
            }
        }
        Command::Config => board::config(),
    }
}

pub(crate) async fn connect_with_session_files(
    endpoint: board::global_config::BoardEndpoint,
    board_type: &str,
    session_files: &[SessionFileArg],
) -> anyhow::Result<()> {
    let (client, session) = board::acquire_board_session_endpoint(endpoint, board_type).await?;
    println!("Allocated board session:");
    println!("  board_type: {board_type}");
    println!("  board_id: {}", session.info().board_id);
    println!("  lease_expires_at: {}", session.info().lease_expires_at);
    println!("  boot_mode: {}", session.info().boot_mode);

    let run_result = async {
        session.context().await?;
        for file in session_files {
            let bytes = tokio::fs::read(&file.local_path).await.with_context(|| {
                format!(
                    "failed to read session file `{}`",
                    file.local_path.display()
                )
            })?;
            let shared = session
                .upload_shared_file(&file.relative_path, bytes)
                .await?;
            println!(
                "  session_file: {} -> {}",
                file.relative_path, shared.http_url
            );
        }

        if !session.info().serial_available {
            anyhow::bail!("board has no serial configuration");
        }
        let ws_path = session
            .info()
            .ws_url
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("server did not return a serial websocket URL"))?;
        let ws_url = client.resolve_ws_url(ws_path)?;
        board::terminal::run_serial_terminal(ws_url, client.websocket_authorization().await?).await
    }
    .await;

    let release_result = session.release().await;
    match (run_result, release_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Err(run_error), Err(release_error)) => Err(run_error.context(format!(
            "failed to release board session: {release_error:#}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::SessionFileArg;

    #[test]
    fn session_file_argument_keeps_server_and_local_paths_explicit() {
        let parsed = "usr/bin/block-rw-bench=/tmp/block-rw-bench"
            .parse::<SessionFileArg>()
            .unwrap();

        assert_eq!(parsed.relative_path, "usr/bin/block-rw-bench");
        assert_eq!(
            parsed.local_path,
            std::path::PathBuf::from("/tmp/block-rw-bench")
        );
    }

    #[test]
    fn session_file_argument_rejects_ambiguous_or_escaping_paths() {
        for value in [
            "missing-separator",
            "=empty-relative",
            "usr/bin/helper=",
            "../helper=/tmp/helper",
            "/usr/bin/helper=/tmp/helper",
        ] {
            assert!(value.parse::<SessionFileArg>().is_err(), "{value}");
        }
    }
}
