#![cfg_attr(not(any(windows, unix)), no_main)]
#![cfg_attr(not(any(windows, unix)), no_std)]

#[cfg(not(any(windows, unix)))]
mod lang;

#[cfg(any(windows, unix))]
#[derive(clap::Parser)]
struct Cli {
    #[command(subcommand)]
    command: axbuild::starry::Command,
}

#[cfg(any(windows, unix))]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use clap::Parser;

    let cli = Cli::parse();
    axbuild::starry::Starry::new()?.execute(cli.command).await?;
    Ok(())
}
