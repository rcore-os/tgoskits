use clap::Subcommand;

use crate::axvisor::context::AxvisorContext;

pub mod context;
pub mod image;

/// Axvisor host-side commands
#[derive(Subcommand)]
pub enum Command {
    /// Guest image management
    Image(image::Args),
}

pub struct Axvisor {
    ctx: AxvisorContext,
}

impl Axvisor {
    pub fn new() -> anyhow::Result<Self> {
        let ctx = AxvisorContext::new()?;
        Ok(Self { ctx })
    }

    pub async fn execute(&mut self, command: Command) -> anyhow::Result<()> {
        match command {
            Command::Image(args) => {
                self.image(args).await?;
            }
        }
        Ok(())
    }

    async fn image(&self, args: image::Args) -> anyhow::Result<()> {
        image::run(args, &self.ctx).await
    }
}

impl Default for Axvisor {
    fn default() -> Self {
        Self::new().expect("failed to initialize Axvisor")
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::context::workspace_root_path;

    #[test]
    fn context_resolves_workspace_root() {
        let ctx = AxvisorContext::new().unwrap();
        assert_eq!(
            ctx.workspace_root(),
            workspace_root_path().unwrap().as_path()
        );
    }

    #[test]
    fn command_parses_image_ls() {
        #[derive(clap::Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from(["axvisor", "image", "ls"]).unwrap();

        match cli.command {
            Command::Image(_) => {}
        }
    }
}
