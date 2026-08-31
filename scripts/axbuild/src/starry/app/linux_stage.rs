use ostool::board::config::BoardRunConfig;

use super::ArgsAppBoard;
use crate::{
    board::{SessionFileArg, connect_with_session_files},
    starry::test::PreparedBoardSessionAssets,
};

/// Upload the prepared app assets and connect to the board's default Linux.
///
/// This path deliberately stops before building or booting Starry. The
/// operator can run and persist the exact uploaded helper from Linux, release
/// the lease, and then start the normal Starry board flow.
pub(in crate::starry) async fn stage_in_default_linux(
    args: &ArgsAppBoard,
    board_config: &BoardRunConfig,
    assets: &PreparedBoardSessionAssets,
) -> anyhow::Result<()> {
    let session_files = assets
        .relative_paths
        .iter()
        .map(|relative_path| {
            let relative_path_text = relative_path.to_str().ok_or_else(|| {
                anyhow::anyhow!(
                    "session file path is not valid UTF-8: {}",
                    relative_path.display()
                )
            })?;
            SessionFileArg::new(
                relative_path_text.to_string(),
                assets.root.join(relative_path),
            )
            .map_err(anyhow::Error::msg)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let global_config = ostool::board::load_board_global_config_with_notice()?;
    let server = args
        .server
        .as_deref()
        .or(board_config.server.as_deref())
        .unwrap_or(&global_config.board.server);
    let port = args.port.or(board_config.port).or(global_config.board.port);
    let auth_mode = board_config
        .auth_mode
        .unwrap_or(global_config.board.auth_mode);
    let endpoint = ostool::board::global_config::BoardEndpoint::new(server, port, auth_mode)?;
    let board_type = args
        .board_type
        .as_deref()
        .unwrap_or(&board_config.board_type);

    connect_with_session_files(endpoint, board_type, &session_files).await
}
