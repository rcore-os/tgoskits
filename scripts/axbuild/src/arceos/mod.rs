use std::{
    collections::BTreeSet,
    fs::File,
    path::{Path, PathBuf},
    process::Command as StdCommand,
};

use anyhow::Context;
use clap::{Args, Subcommand};
use log::warn;
use ostool::{build::config::Cargo, run::qemu::QemuConfig};

use crate::{
    context::{AppContext, BuildCliArgs, ResolvedBuildRequest, SnapshotPersistence},
    test::host_http::HostHttpServerGuard,
};

const DEFAULT_TEST_DISK_IMAGE_SIZE: &str = "64M";

/// Prepare runtime disk images referenced by QEMU configs.
pub(super) fn ensure_qemu_runtime_assets(
    workspace_root: &Path,
    qemu: &QemuConfig,
) -> anyhow::Result<()> {
    let mut seen = BTreeSet::new();
    for image in qemu_runtime_disk_images(workspace_root, qemu) {
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

    if command_exists("truncate") {
        ran(StdCommand::new("truncate").args(["-s", size]).arg(image))?;
    } else {
        let bytes = parse_size_to_bytes(size)?;
        let file = File::create(image)
            .with_context(|| format!("failed to create runtime image {}", image.display()))?;
        file.set_len(bytes)
            .with_context(|| format!("failed to resize runtime image {}", image.display()))?;
    }

    if command_exists("mkfs.fat") {
        ran(StdCommand::new("mkfs.fat")
            .args(["-F", "32"])
            .arg(image)
            .stdout(std::process::Stdio::null()))?;
    } else {
        warn!(
            "`mkfs.fat` not found in PATH; runtime disk image {} will be an unformatted raw file",
            image.display()
        );
    }

    println!("{msg} ... done");
    Ok(())
}

fn parse_size_to_bytes(size: &str) -> anyhow::Result<u64> {
    let trimmed = size.trim();
    let number = trimmed
        .trim_end_matches(|c: char| c.is_ascii_alphabetic())
        .trim();
    let value: u64 = number
        .parse()
        .with_context(|| format!("invalid disk image size `{size}`"))?;

    let suffix = trimmed[number.len()..].trim().to_ascii_lowercase();
    let unit = match suffix.as_str() {
        "" | "b" => 1,
        "k" | "kb" => 1024,
        "m" | "mb" => 1024 * 1024,
        "g" | "gb" => 1024 * 1024 * 1024,
        _ => {
            anyhow::bail!("unsupported disk image size suffix `{suffix}` in `{size}`");
        }
    };

    value
        .checked_mul(unit)
        .with_context(|| format!("disk image size `{size}` is too large"))
}

fn command_exists(command: &str) -> bool {
    let command_name = format!("{command}{}", std::env::consts::EXE_SUFFIX);
    std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path).any(|dir| {
                let candidate = dir.join(&command_name);
                candidate.is_file()
            })
        })
        .unwrap_or(false)
}

fn qemu_runtime_disk_images(workspace_root: &Path, qemu: &QemuConfig) -> Vec<PathBuf> {
    crate::rootfs::qemu::drive_file_paths(qemu)
        .into_iter()
        .map(|path| expand_workspace_path(workspace_root, &path))
        .filter(|path| path.file_name().and_then(|name| name.to_str()) == Some("disk.img"))
        .collect()
}

fn expand_workspace_path(workspace_root: &Path, path: &Path) -> PathBuf {
    let Some(raw) = path.to_str() else {
        return path.to_path_buf();
    };

    if raw == "${workspace}" {
        return workspace_root.to_path_buf();
    }

    if let Some(relative) = raw.strip_prefix("${workspace}/") {
        return workspace_root.join(relative);
    }

    path.to_path_buf()
}

fn should_recreate_runtime_image(workspace_root: &Path, image: &Path) -> bool {
    image.starts_with(crate::context::axbuild_tmp_dir(workspace_root).join("runtime-assets"))
}

pub mod build;
pub mod cbuild;
pub mod rootfs;
pub mod test;

fn start_qemu_host_http_server(
    request: &ResolvedBuildRequest,
) -> anyhow::Result<Option<HostHttpServerGuard>> {
    request
        .qemu_config
        .as_deref()
        .map(crate::test::qemu::load_qemu_case_host_http_server)
        .transpose()?
        .flatten()
        .map(|config| HostHttpServerGuard::start(&config, &request.package))
        .transpose()
}

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
        let mut qemu = match request.qemu_config.as_deref() {
            Some(path) => self
                .app
                .read_qemu_config_from_path_for_cargo(cargo, path)
                .await
                .map(Some)?,
            None => {
                let default_path = self
                    .app
                    .workspace_member_dir(&request.package)?
                    .join(format!("qemu-{}.toml", request.arch));
                if default_path.exists() {
                    self.app
                        .read_qemu_config_from_path_for_cargo(cargo, &default_path)
                        .await
                        .map(Some)?
                } else {
                    None
                }
            }
        };
        if let Some(qemu) = qemu.as_mut() {
            crate::test::qemu::apply_dynamic_platform_qemu_boot(qemu, cargo);
        }
        Ok(qemu)
    }

    async fn load_uboot_config(
        &mut self,
        request: &ResolvedBuildRequest,
        cargo: &Cargo,
    ) -> anyhow::Result<Option<ostool::run::uboot::UbootConfig>> {
        match request.uboot_config.as_deref() {
            Some(path) => self
                .app
                .read_uboot_config_from_path_for_cargo(cargo, path)
                .await
                .map(Some),
            None => Ok(None),
        }
    }

    async fn run_qemu_request(&mut self, request: ResolvedBuildRequest) -> anyhow::Result<()> {
        match build::load_arceos_build_mode(&request.build_info_path)? {
            build::ArceosBuildMode::RustStd => {
                let cargo = build::load_cargo_config(&request)?;
                self.run_qemu_request_with_cargo(request, cargo).await
            }
            build::ArceosBuildMode::AppC { app_dir, app_name } => {
                self.run_c_app_qemu_request(request, app_dir, app_name)
                    .await
            }
        }
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
        let _host_http_server = start_qemu_host_http_server(&request)?;
        self.app.qemu(cargo, request.build_info_path, qemu).await
    }

    async fn run_build_request(&mut self, request: ResolvedBuildRequest) -> anyhow::Result<()> {
        self.app.set_debug_mode(request.debug)?;
        match build::load_arceos_build_mode(&request.build_info_path)? {
            build::ArceosBuildMode::RustStd => {
                let cargo = build::load_cargo_config(&request)?;
                self.app.build(cargo, request.build_info_path).await
            }
            build::ArceosBuildMode::AppC { app_dir, app_name } => {
                let request = c_app_internal_request(&request);
                let output = self.build_c_app_request(&request, app_dir, app_name)?;
                println!("Built ArceOS C app ELF: {}", output.elf_path.display());
                Ok(())
            }
        }
    }

    async fn run_uboot_request(&mut self, request: ResolvedBuildRequest) -> anyhow::Result<()> {
        self.app.set_debug_mode(request.debug)?;
        match build::load_arceos_build_mode(&request.build_info_path)? {
            build::ArceosBuildMode::RustStd => {
                let cargo = build::load_cargo_config(&request)?;
                let uboot = self.load_uboot_config(&request, &cargo).await?;
                self.app.uboot(cargo, request.build_info_path, uboot).await
            }
            build::ArceosBuildMode::AppC { app_dir, app_name } => {
                self.run_c_app_uboot_request(request, app_dir, app_name)
                    .await
            }
        }
    }

    fn build_c_app_request(
        &mut self,
        request: &ResolvedBuildRequest,
        app_dir: PathBuf,
        app_name: String,
    ) -> anyhow::Result<cbuild::ArceosCBuildOutput> {
        let workspace_root = self.app.workspace_root();
        let config = build::load_arceos_build_config(&request.build_info_path)?;
        let paths = cbuild::default_c_app_artifact_paths(workspace_root, &app_name);
        let input = cbuild::ArceosCBuildInput {
            app_dir,
            app_name,
            target_dir: paths.target_dir,
            out_dir: paths.out_dir,
            features: config.build_info.features,
        };

        cbuild::build_c_app(workspace_root, request, &input)
    }

    async fn run_c_app_qemu_request(
        &mut self,
        request: ResolvedBuildRequest,
        app_dir: PathBuf,
        app_name: String,
    ) -> anyhow::Result<()> {
        self.app.set_debug_mode(request.debug)?;
        let request = c_app_internal_request(&request);
        let cargo = build::load_c_app_cargo_config(&request)?;
        let mut qemu = self
            .load_qemu_config(&request, &cargo)
            .await?
            .with_context(|| {
                format!(
                    "ArceOS C app config {} requires an explicit qemu config",
                    request.build_info_path.display()
                )
            })?;
        let output = self.build_c_app_request(&request, app_dir, app_name)?;
        crate::test::qemu::apply_dynamic_platform_qemu_boot(&mut qemu, &cargo);
        ensure_qemu_runtime_assets(self.app.workspace_root(), &qemu)?;
        self.app
            .prepare_elf_artifact(output.elf_path, qemu.to_bin)
            .await?;
        let _host_http_server = start_qemu_host_http_server(&request)?;
        self.app.run_prepared_qemu(qemu, None).await
    }

    async fn run_c_app_uboot_request(
        &mut self,
        request: ResolvedBuildRequest,
        app_dir: PathBuf,
        app_name: String,
    ) -> anyhow::Result<()> {
        self.app.set_debug_mode(request.debug)?;
        let request = c_app_internal_request(&request);
        let cargo = build::load_c_app_cargo_config(&request)?;
        let uboot = self
            .load_uboot_config(&request, &cargo)
            .await?
            .with_context(|| {
                format!(
                    "ArceOS C app config {} requires an explicit uboot config",
                    request.build_info_path.display()
                )
            })?;
        let output = self.build_c_app_request(&request, app_dir, app_name)?;
        self.app.prepare_elf_artifact(output.elf_path, true).await?;
        self.app.run_prepared_uboot(uboot).await
    }
}

fn warn_if_c_app_package_override(request: &ResolvedBuildRequest) {
    if request.package != "ax-libc" {
        warn!(
            "ArceOS C app build ignores --package {}; using ax-libc internally",
            request.package
        );
    }
}

fn c_app_internal_request(request: &ResolvedBuildRequest) -> ResolvedBuildRequest {
    warn_if_c_app_package_override(request);
    let mut request = request.clone();
    request.package = "ax-libc".to_string();
    request
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn qemu_runtime_disk_images_finds_disk_img_drive_paths() {
        let workspace = Path::new("/workspace");
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
            qemu_runtime_disk_images(workspace, &qemu),
            vec![PathBuf::from("/tmp/case/disk.img")]
        );
    }

    #[test]
    fn qemu_runtime_disk_images_expands_workspace_placeholder() {
        let workspace = Path::new("/workspace");
        let qemu = QemuConfig {
            args: vec![
                "-drive".to_string(),
                "id=disk0,if=none,format=raw,file=${workspace}/tmp/axbuild/runtime-assets/apps/\
                 arceos/helloworld/disk.img"
                    .to_string(),
            ],
            ..Default::default()
        };

        assert_eq!(
            qemu_runtime_disk_images(workspace, &qemu),
            vec![PathBuf::from(
                "/workspace/tmp/axbuild/runtime-assets/apps/arceos/helloworld/disk.img"
            )]
        );
    }

    #[test]
    fn should_recreate_only_tmp_runtime_asset_images() {
        let workspace = Path::new("/workspace");

        assert!(should_recreate_runtime_image(
            workspace,
            Path::new("/workspace/tmp/axbuild/runtime-assets/apps/arceos/std/case/disk.img")
        ));
        assert!(!should_recreate_runtime_image(
            workspace,
            Path::new("/workspace/test-suit/arceos/rust/fs/shell/disk.img")
        ));
    }

    #[test]
    fn qemu_request_starts_host_http_server_from_config() {
        let root = tempdir().unwrap();
        let qemu_config = root.path().join("qemu-x86_64.toml");
        std::fs::write(
            &qemu_config,
            r#"
args = []

[host_http_server]
port = 0
body = "fixture"
"#,
        )
        .unwrap();
        let request = ResolvedBuildRequest {
            package: "arceos-httpclient".to_string(),
            arch: "x86_64".to_string(),
            target: "x86_64-unknown-none".to_string(),
            plat_dyn: Some(true),
            smp: Some(1),
            debug: false,
            build_info_path: root.path().join("build.toml"),
            qemu_config: Some(qemu_config),
            uboot_config: None,
        };

        let guard = start_qemu_host_http_server(&request).unwrap();

        assert!(guard.is_some());
    }
}
