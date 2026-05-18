use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    process::Command as StdCommand,
};

use anyhow::Context;
use clap::{Args, Subcommand};
use ostool::{build::config::Cargo, run::qemu::QemuConfig};

use crate::context::{AppContext, BuildCliArgs, ResolvedBuildRequest, SnapshotPersistence};

const DEFAULT_TEST_DISK_IMAGE_SIZE: &str = "64M";

/// Prepare runtime disk images referenced by QEMU configs.
pub(super) fn ensure_qemu_runtime_assets(
    workspace_root: &Path,
    qemu: &QemuConfig,
) -> anyhow::Result<()> {
    let mut seen = BTreeSet::new();
    for image in qemu_runtime_disk_images(qemu) {
        if !seen.insert(image.clone()) {
            continue;
        }
        ensure_fat32_image(
            &image,
            DEFAULT_TEST_DISK_IMAGE_SIZE,
            should_recreate_runtime_image(workspace_root, &image),
        )?;
    }
    Ok(())
}

/// Create a FAT32 disk image at `path` with the given `size` if it does not
/// already exist.
fn ensure_fat32_image(image: &Path, size: &str, recreate: bool) -> anyhow::Result<()> {
    if image.exists() && !recreate {
        return Ok(());
    }
    let msg = format!("generating {}", image.display());
    println!("{msg} ...");
    if let Some(parent) = image.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if image.exists() {
        std::fs::remove_file(image)?;
    }
    let ran = |cmd: &mut StdCommand| -> anyhow::Result<()> {
        let name = cmd.get_program().to_string_lossy().to_string();
        cmd.status()
            .with_context(|| format!("failed to run `{name}`"))?
            .success()
            .then_some(())
            .ok_or_else(|| anyhow::anyhow!("`{name}` exited with non-zero status"))
    };
    ran(StdCommand::new("truncate").args(["-s", size]).arg(image))?;
    ran(StdCommand::new("mkfs.fat")
        .args(["-F", "32"])
        .arg(image)
        .stdout(std::process::Stdio::null()))?;
    println!("{msg} ... done");
    Ok(())
}

fn qemu_runtime_disk_images(qemu: &QemuConfig) -> Vec<PathBuf> {
    crate::rootfs::qemu::drive_file_paths(qemu)
        .into_iter()
        .filter(|path| path.file_name().and_then(|name| name.to_str()) == Some("disk.img"))
        .collect()
}

fn should_recreate_runtime_image(workspace_root: &Path, image: &Path) -> bool {
    image.starts_with(crate::context::axbuild_tmp_dir(workspace_root).join("runtime-assets"))
}

pub mod build;
pub mod cbuild;
pub mod rootfs;
pub mod test;

// ---------------------------------------------------------------------------
// CLI types
// ---------------------------------------------------------------------------

/// ArceOS subcommands
#[derive(Subcommand)]
pub enum Command {
    /// Build ArceOS application
    Build(ArgsBuild),
    /// Build and run ArceOS application in QEMU
    Qemu(ArgsQemu),
    /// Run ArceOS test suites
    Test(test::ArgsTest),
    /// Build and run ArceOS application with U-Boot
    Uboot(ArgsUboot),
}

#[derive(Args)]
pub struct ArgsBuild {
    #[arg(short, long)]
    pub config: Option<PathBuf>,
    #[arg(short, long)]
    pub package: Option<String>,
    #[arg(long)]
    pub arch: Option<String>,
    #[arg(short, long)]
    pub target: Option<String>,
    #[arg(long = "plat_dyn", alias = "plat-dyn")]
    pub plat_dyn: Option<bool>,

    #[arg(long, value_name = "CPUS")]
    pub smp: Option<usize>,

    #[arg(long)]
    pub debug: bool,
}

#[derive(Args)]
pub struct ArgsQemu {
    #[command(flatten)]
    pub build: ArgsBuild,

    #[arg(long)]
    pub qemu_config: Option<PathBuf>,

    /// Override the rootfs disk image path (skips auto-download).
    #[arg(long, value_name = "IMAGE")]
    pub rootfs: Option<PathBuf>,
}

#[derive(Args)]
pub struct ArgsUboot {
    #[command(flatten)]
    pub build: ArgsBuild,

    #[arg(long)]
    pub uboot_config: Option<PathBuf>,
}

// ---------------------------------------------------------------------------
// ArceOS executor
// ---------------------------------------------------------------------------

pub struct ArceOS {
    pub(super) app: AppContext,
}

impl From<&ArgsBuild> for BuildCliArgs {
    fn from(args: &ArgsBuild) -> Self {
        Self {
            config: args.config.clone(),
            package: args.package.clone(),
            arch: args.arch.clone(),
            target: args.target.clone(),
            plat_dyn: args.plat_dyn,
            smp: args.smp,
            debug: args.debug,
        }
    }
}

impl ArceOS {
    pub fn new() -> anyhow::Result<Self> {
        let app = AppContext::new()?;
        Ok(Self { app })
    }

    pub async fn execute(&mut self, command: Command) -> anyhow::Result<()> {
        match command {
            Command::Build(args) => self.build(args).await,
            Command::Qemu(args) => self.qemu(args).await,
            Command::Uboot(args) => self.uboot(args).await,
            Command::Test(args) => self.test(args).await,
        }
    }

    async fn build(&mut self, args: ArgsBuild) -> anyhow::Result<()> {
        let request =
            self.prepare_request((&args).into(), None, None, SnapshotPersistence::Store)?;
        self.run_build_request(request).await
    }

    async fn qemu(&mut self, args: ArgsQemu) -> anyhow::Result<()> {
        let request = self.prepare_request(
            (&args.build).into(),
            args.qemu_config,
            None,
            SnapshotPersistence::Store,
        )?;
        if let Some(rootfs) = args.rootfs {
            rootfs::qemu_with_explicit_rootfs(self, request, rootfs).await
        } else {
            self.run_qemu_request(request).await
        }
    }

    async fn uboot(&mut self, args: ArgsUboot) -> anyhow::Result<()> {
        let request = self.prepare_request(
            (&args.build).into(),
            None,
            args.uboot_config,
            SnapshotPersistence::Store,
        )?;
        self.run_uboot_request(request).await
    }

    // ---- test dispatch ----

    async fn test(&mut self, args: test::ArgsTest) -> anyhow::Result<()> {
        test::test(self, args).await
    }

    // ---- internal helpers ----

    pub(super) fn prepare_request(
        &self,
        args: BuildCliArgs,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
        persistence: SnapshotPersistence,
    ) -> anyhow::Result<ResolvedBuildRequest> {
        let (request, snapshot) = self.app.prepare_arceos_request(
            args,
            qemu_config,
            uboot_config,
            build::resolve_build_info_path,
        )?;
        if persistence.should_store() {
            self.app.store_arceos_snapshot(&snapshot)?;
        }
        Ok(request)
    }

    pub(super) async fn load_qemu_config(
        &mut self,
        request: &ResolvedBuildRequest,
        cargo: &Cargo,
    ) -> anyhow::Result<Option<ostool::run::qemu::QemuConfig>> {
        match request.qemu_config.as_deref() {
            Some(path) => self
                .app
                .tool_mut()
                .read_qemu_config_from_path_for_cargo(cargo, path)
                .await
                .map(Some),
            None => Ok(None),
        }
    }

    async fn load_uboot_config(
        &mut self,
        request: &ResolvedBuildRequest,
        cargo: &Cargo,
    ) -> anyhow::Result<Option<ostool::run::uboot::UbootConfig>> {
        match request.uboot_config.as_deref() {
            Some(path) => self
                .app
                .tool_mut()
                .read_uboot_config_from_path_for_cargo(cargo, path)
                .await
                .map(Some),
            None => Ok(None),
        }
    }

    async fn run_qemu_request(&mut self, request: ResolvedBuildRequest) -> anyhow::Result<()> {
        let cargo = build::load_cargo_config(&request)?;
        self.run_qemu_request_with_cargo(request, cargo).await
    }

    async fn run_qemu_request_with_cargo(
        &mut self,
        request: ResolvedBuildRequest,
        cargo: Cargo,
    ) -> anyhow::Result<()> {
        self.app.set_debug_mode(request.debug)?;
        let qemu = self.load_qemu_config(&request, &cargo).await?;
        if let Some(qemu) = qemu.as_ref() {
            ensure_qemu_runtime_assets(self.app.workspace_root(), qemu)?;
        }
        self.app.qemu(cargo, request.build_info_path, qemu).await
    }

    async fn run_build_request(&mut self, request: ResolvedBuildRequest) -> anyhow::Result<()> {
        self.app.set_debug_mode(request.debug)?;
        let cargo = build::load_cargo_config(&request)?;
        self.app.build(cargo, request.build_info_path).await
    }

    async fn run_uboot_request(&mut self, request: ResolvedBuildRequest) -> anyhow::Result<()> {
        self.app.set_debug_mode(request.debug)?;
        let cargo = build::load_cargo_config(&request)?;
        let uboot = self.load_uboot_config(&request, &cargo).await?;
        self.app.uboot(cargo, request.build_info_path, uboot).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qemu_runtime_disk_images_finds_disk_img_drive_paths() {
        let qemu = QemuConfig {
            args: vec![
                "-drive".to_string(),
                "id=disk0,if=none,format=raw,file=/tmp/case/disk.img".to_string(),
                "-drive".to_string(),
                "id=rootfs,if=none,format=raw,file=/tmp/rootfs.img".to_string(),
            ],
            ..Default::default()
        };

        assert_eq!(
            qemu_runtime_disk_images(&qemu),
            vec![PathBuf::from("/tmp/case/disk.img")]
        );
    }

    #[test]
    fn should_recreate_only_tmp_runtime_asset_images() {
        let workspace = Path::new("/workspace");

        assert!(should_recreate_runtime_image(
            workspace,
            Path::new("/workspace/tmp/axbuild/runtime-assets/arceos/std/case/disk.img")
        ));
        assert!(!should_recreate_runtime_image(
            workspace,
            Path::new("/workspace/test-suit/arceos/rust/fs/shell/disk.img")
        ));
    }
}
