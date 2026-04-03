use clap::{Args, Subcommand};
use ostool::board::{self, global_config::LoadedBoardGlobalConfig};

#[derive(Subcommand, Debug)]
pub enum Command {
    /// List available remote board types
    Ls(BoardServerArgs),
    /// Allocate a remote board and connect to its serial terminal
    Connect(ArgsConnect),
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

    #[command(flatten)]
    pub server: BoardServerArgs,
}

pub async fn execute(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Ls(server) => {
            let global_config = load_board_global_config_with_notice()?;
            let (server, port) =
                global_config.resolve_server(server.server.as_deref(), server.port);
            board::list_boards(&server, port).await
        }
        Command::Connect(args) => {
            let global_config = load_board_global_config_with_notice()?;
            let (server, port) =
                global_config.resolve_server(args.server.server.as_deref(), args.server.port);
            board::connect_board(&server, port, &args.board_type).await
        }
    }
}

fn load_board_global_config_with_notice() -> anyhow::Result<LoadedBoardGlobalConfig> {
    let loaded = LoadedBoardGlobalConfig::load_or_create()?;
    if loaded.created {
        println!("Created default board config: {}", loaded.path.display());
    }
    Ok(loaded)
}
