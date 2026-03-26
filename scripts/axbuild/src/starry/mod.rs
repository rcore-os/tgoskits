use std::path::PathBuf;

use clap::{Args, Subcommand};
use ostool::build::CargoQemuOverrideArgs;

use crate::context::{
    AppContext, DEFAULT_STARRY_ARCH, QemuRunConfig, StarryCliArgs, starry_target_for_arch_checked,
};

pub mod build;
pub mod rootfs;

/// StarryOS subcommands
#[derive(Subcommand)]
pub enum Command {
    /// Build StarryOS application
    Build(ArgsBuild),
    /// Build and run StarryOS application
    Qemu(ArgsQemu),
    /// Download rootfs image into workspace target directory
    Rootfs(ArgsRootfs),

    /// Build and run StarryOS application with U-Boot
    Uboot(ArgsUboot),
}

#[derive(Args, Clone)]
pub struct ArgsBuild {
    #[arg(short, long)]
    pub config: Option<PathBuf>,
    #[arg(long)]
    pub arch: Option<String>,
    #[arg(short, long)]
    pub target: Option<String>,
    #[arg(long = "plat_dyn", alias = "plat-dyn")]
    pub plat_dyn: Option<bool>,
}

#[derive(Args)]
pub struct ArgsQemu {
    #[command(flatten)]
    pub build: ArgsBuild,

    #[arg(long)]
    pub qemu_config: Option<PathBuf>,
}

#[derive(Args)]
pub struct ArgsUboot {
    #[command(flatten)]
    pub build: ArgsBuild,

    #[arg(long)]
    pub uboot_config: Option<PathBuf>,
}

#[derive(Args)]
pub struct ArgsRootfs {
    #[arg(long)]
    pub arch: Option<String>,
}

pub struct Starry {
    app: AppContext,
}

impl From<&ArgsBuild> for StarryCliArgs {
    fn from(args: &ArgsBuild) -> Self {
        Self {
            config: args.config.clone(),
            arch: args.arch.clone(),
            target: args.target.clone(),
            plat_dyn: args.plat_dyn,
        }
    }
}

impl Starry {
    pub fn new() -> anyhow::Result<Self> {
        let app = AppContext::new()?;
        Ok(Self { app })
    }

    pub async fn execute(&mut self, command: Command) -> anyhow::Result<()> {
        match command {
            Command::Build(args) => {
                self.build(args).await?;
            }
            Command::Qemu(args) => {
                self.qemu(args).await?;
            }
            Command::Rootfs(args) => {
                self.rootfs(args).await?;
            }
            Command::Uboot(args) => {
                self.uboot(args).await?;
            }
        }
        Ok(())
    }

    async fn build(&mut self, args: ArgsBuild) -> anyhow::Result<()> {
        let (request, snapshot) = self
            .app
            .prepare_starry_request((&args).into(), None, None)?;
        self.app.store_starry_snapshot(&snapshot)?;

        let cargo = build::load_cargo_config(&request)?;
        self.app.build(cargo, request.build_info_path).await?;
        Ok(())
    }

    async fn qemu(&mut self, args: ArgsQemu) -> anyhow::Result<()> {
        let (request, snapshot) = self.app.prepare_starry_request(
            (&args.build).into(),
            args.qemu_config.clone(),
            None,
        )?;
        self.app.store_starry_snapshot(&snapshot)?;

        let cargo = build::load_cargo_config(&request)?;
        let qemu_args = rootfs::default_qemu_args(self.app.workspace_root(), &request).await?;
        self.app
            .qemu(
                cargo,
                request.build_info_path,
                QemuRunConfig {
                    qemu_config: request.qemu_config,
                    default_args: CargoQemuOverrideArgs {
                        args: Some(qemu_args),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .await?;
        Ok(())
    }

    async fn rootfs(&mut self, args: ArgsRootfs) -> anyhow::Result<()> {
        let arch = args.arch.unwrap_or_else(|| DEFAULT_STARRY_ARCH.to_string());
        let target = starry_target_for_arch_checked(&arch)?.to_string();
        let disk_img =
            rootfs::ensure_rootfs_in_target_dir(self.app.workspace_root(), &arch, &target).await?;
        println!("rootfs ready at {}", disk_img.display());
        Ok(())
    }

    async fn uboot(&mut self, args: ArgsUboot) -> anyhow::Result<()> {
        let (request, snapshot) = self.app.prepare_starry_request(
            (&args.build).into(),
            None,
            args.uboot_config.clone(),
        )?;
        self.app.store_starry_snapshot(&snapshot)?;

        let cargo = build::load_cargo_config(&request)?;
        self.app
            .uboot(cargo, request.build_info_path, request.uboot_config)
            .await?;
        Ok(())
    }
}

impl Default for Starry {
    fn default() -> Self {
        Self::new().expect("failed to initialize StarryOS")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_args_convert_to_cli_args() {
        let args = ArgsBuild {
            config: Some(PathBuf::from("/tmp/starry.toml")),
            arch: Some("aarch64".to_string()),
            target: Some("aarch64-unknown-none-softfloat".to_string()),
            plat_dyn: Some(false),
        };

        let cli_args = StarryCliArgs::from(&args);

        assert_eq!(
            cli_args,
            StarryCliArgs {
                config: Some(PathBuf::from("/tmp/starry.toml")),
                arch: Some("aarch64".to_string()),
                target: Some("aarch64-unknown-none-softfloat".to_string()),
                plat_dyn: Some(false),
            }
        );
    }

    #[test]
    fn qemu_and_uboot_args_keep_extra_paths() {
        let build = ArgsBuild {
            config: None,
            arch: Some("x86_64".to_string()),
            target: Some("x86_64-unknown-none".to_string()),
            plat_dyn: Some(true),
        };
        let qemu = ArgsQemu {
            build: build.clone(),
            qemu_config: Some(PathBuf::from("qemu.toml")),
        };
        let uboot = ArgsUboot {
            build,
            uboot_config: Some(PathBuf::from("uboot.toml")),
        };

        assert_eq!(qemu.qemu_config, Some(PathBuf::from("qemu.toml")));
        assert_eq!(uboot.uboot_config, Some(PathBuf::from("uboot.toml")));
        assert_eq!(qemu.build.arch.as_deref(), Some("x86_64"));
        assert_eq!(uboot.build.target.as_deref(), Some("x86_64-unknown-none"));
        assert_eq!(qemu.build.plat_dyn, Some(true));
    }

    #[test]
    fn rootfs_args_allow_arch_override() {
        let args = ArgsRootfs {
            arch: Some("riscv64".to_string()),
        };

        assert_eq!(args.arch.as_deref(), Some("riscv64"));
    }

    #[test]
    fn starry_qemu_uses_default_args_for_disk_and_net() {
        let qemu = QemuRunConfig {
            qemu_config: Some(PathBuf::from("qemu.toml")),
            default_args: CargoQemuOverrideArgs {
                args: Some(vec![
                    "-device".to_string(),
                    "virtio-blk-pci,drive=disk0".to_string(),
                ]),
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(qemu.default_args.args.is_some());
        assert!(qemu.append_args.args.is_none());
    }
}
