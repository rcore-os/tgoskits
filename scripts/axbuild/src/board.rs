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

    #[command(flatten)]
    pub server: BoardServerArgs,
}

pub async fn execute(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Ls(server) => {
            let global_config = board::load_board_global_config_with_notice()?;
            let (server, port) =
                global_config.resolve_server(server.server.as_deref(), server.port);
            let boards = board::fetch_board_types(&server, port).await?;
            println!("{}", board::render_board_table(&boards));
            Ok(())
        }
        Command::Connect(args) => {
            let global_config = board::load_board_global_config_with_notice()?;
            let (server, port) =
                global_config.resolve_server(args.server.server.as_deref(), args.server.port);
            board::connect_board(&server, port, &args.board_type).await
        }
        Command::Config => board::config(),
    }
}
