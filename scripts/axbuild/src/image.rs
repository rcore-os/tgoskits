use std::{
    fs,
    path::{Path, PathBuf},
};

use clap::{Args as ClapArgs, Subcommand};

use crate::{context::AppContext, support::download::file_sha256};

pub mod config;
pub mod registry;
pub mod spec;
pub mod storage;

use config::ImageConfig;
use spec::ImageSpecRef;
use storage::Storage;

#[derive(ClapArgs)]
pub struct ImageArgs {
    #[command(flatten)]
    pub overrides: ConfigOverrides,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(ClapArgs, Debug, Clone, Default)]
pub struct ConfigOverrides {
    #[arg(short('S'), long, global = true)]
    pub local_storage: Option<PathBuf>,

    #[arg(short('R'), long, global = true)]
    pub registry: Option<String>,

    #[arg(short('N'), long, global = true)]
    pub no_auto_sync: bool,

    #[arg(long, global = true)]
    pub auto_sync_threshold: Option<u64>,
}

impl ConfigOverrides {
    pub fn apply_on(&self, config: &mut ImageConfig) {
        if let Some(local_storage) = self.local_storage.as_ref() {
            config.local_storage = local_storage.clone();
        }
        if let Some(registry) = self.registry.as_ref() {
            config.registry = registry.clone();
        }
        if self.no_auto_sync {
            config.auto_sync = false;
        }
        if let Some(auto_sync_threshold) = self.auto_sync_threshold {
            config.auto_sync_threshold = auto_sync_threshold;
        }
    }
}

#[derive(Subcommand)]
pub enum Command {
    /// List available images from rcore-os/tgosimages registry.
    Ls(ArgsLs),
    /// Pull an image and verify its sha256 checksum.
    Pull(ArgsPull),
    /// Resize an ext rootfs image, optionally copying it first.
    Resize(ArgsResize),
    /// Print and optionally verify the sha256 of a local image.
    Check(ArgsCheck),
}

#[derive(ClapArgs)]
pub struct ArgsLs {
    #[arg(short, long)]
    pub verbose: bool,

    pub pattern: Option<String>,
}

#[derive(ClapArgs)]
pub struct ArgsPull {
    /// Rootfs image name, optionally with `:<version>`.
    ///
    /// Examples: `rootfs-riscv64-alpine.img`, `rootfs-aarch64-alpine.img:v0.0.5`.
    pub image: Option<String>,

    /// Pull the default Starry/ArceOS rootfs for this architecture.
    #[arg(long)]
    pub arch: Option<String>,

    /// Output directory for generic extracted images. Managed rootfs images use local image storage.
    #[arg(short, long)]
    pub output_dir: Option<PathBuf>,

    /// Keep only the downloaded archive for generic images.
    #[arg(long)]
    pub no_extract: bool,
}

#[derive(ClapArgs)]
pub struct ArgsCheck {
    pub image: PathBuf,

    #[arg(long)]
    pub sha256: Option<String>,
}

#[derive(ClapArgs)]
pub struct ArgsResize {
    /// Rootfs image to resize.
    pub image: PathBuf,

    /// Output image path. When omitted, resize IMAGE in place.
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Final image size in MiB. Shrinking is rejected.
    #[arg(long = "size-mib", value_name = "MIB")]
    pub size_mib: u64,
}

pub(crate) async fn run(args: ImageArgs) -> anyhow::Result<()> {
    execute(args).await
}

async fn execute(args: ImageArgs) -> anyhow::Result<()> {
    let app = AppContext::new()?;
    match args.command {
        Command::Ls(ls) => list_images(app.workspace_root(), &args.overrides, ls).await,
        Command::Pull(pull) => pull_image(app.workspace_root(), &args.overrides, pull).await,
        Command::Resize(resize) => resize_image(resize),
        Command::Check(check) => {
            let path = to_absolute_path(&check.image)?;
            let ok = check_image(&path, check.sha256.as_deref())?;
            if ok {
                Ok(())
            } else {
                anyhow::bail!("checksum mismatch for {}", path.display())
            }
        }
    }
}

fn check_image(path: &Path, expected_sha256: Option<&str>) -> anyhow::Result<bool> {
    let actual = file_sha256(path)?;
    if let Some(expected) = expected_sha256 {
        let matches = actual == expected;
        println!(
            "{}  {}{}",
            actual,
            path.display(),
            if matches { "" } else { " (mismatch)" }
        );
        return Ok(matches);
    }

    println!("{actual}  {}", path.display());
    Ok(true)
}

async fn list_images(
    workspace_root: &Path,
    overrides: &ConfigOverrides,
    args: ArgsLs,
) -> anyhow::Result<()> {
    let mut config = ImageConfig::read_config(workspace_root)?;
    overrides.apply_on(&mut config);
    let storage = Storage::new_from_config(&config).await?;
    storage
        .image_registry
        .print(args.verbose, args.pattern.as_deref());
    Ok(())
}

async fn pull_image(
    workspace_root: &Path,
    overrides: &ConfigOverrides,
    args: ArgsPull,
) -> anyhow::Result<()> {
    let image_path = match (args.image.as_deref(), args.arch.as_deref()) {
        (Some(image), None) if args.output_dir.is_none() && !args.no_extract => {
            let mut config = ImageConfig::read_config(workspace_root)?;
            overrides.apply_on(&mut config);
            let storage = Storage::new_from_config(&config).await?;
            match storage.pull_rootfs_image(ImageSpecRef::parse(image)).await {
                Ok(path) => path,
                Err(rootfs_err) => storage
                    .pull_image(ImageSpecRef::parse(image), None, true)
                    .await
                    .map_err(|generic_err| {
                        anyhow::anyhow!(
                            "failed to pull `{image}` as managed rootfs ({rootfs_err}) or generic \
                             image ({generic_err})"
                        )
                    })?,
            }
        }
        (Some(image), None) => {
            let mut config = ImageConfig::read_config(workspace_root)?;
            overrides.apply_on(&mut config);
            let storage = Storage::new_from_config(&config).await?;
            let output_dir = args
                .output_dir
                .as_deref()
                .map(to_absolute_path)
                .transpose()?;
            storage
                .pull_image(
                    ImageSpecRef::parse(image),
                    output_dir.as_deref(),
                    !args.no_extract,
                )
                .await?
        }
        (None, Some(arch)) if args.output_dir.is_none() && !args.no_extract => {
            let mut config = ImageConfig::read_config(workspace_root)?;
            overrides.apply_on(&mut config);
            let image = storage::default_rootfs_image(arch).ok_or_else(|| {
                anyhow::anyhow!("no managed rootfs image available for arch `{arch}`")
            })?;
            let storage = Storage::new_from_config(&config).await?;
            storage.pull_rootfs_image(image.into()).await?
        }
        (None, Some(_)) => {
            anyhow::bail!(
                "`--arch` managed rootfs pulls do not accept `--output-dir` or `--no-extract`"
            )
        }
        (None, None) => {
            anyhow::bail!("provide an image name or use `--arch <ARCH>`")
        }
        (Some(_), Some(_)) => {
            anyhow::bail!(
                "`cargo xtask image pull` accepts either an image name or `--arch`, not both"
            )
        }
    };

    println!("image ready at {}", image_path.display());
    Ok(())
}

fn resize_image(args: ArgsResize) -> anyhow::Result<()> {
    let input = to_absolute_path(&args.image)?;
    let image = match args.output.as_deref() {
        Some(output) => {
            let output = to_absolute_path(output)?;
            if let Some(parent) = output.parent() {
                fs::create_dir_all(parent).map_err(|err| {
                    anyhow::anyhow!("failed to create {}: {err}", parent.display())
                })?;
            }
            fs::copy(&input, &output).map_err(|err| {
                anyhow::anyhow!(
                    "failed to copy {} to {}: {err}",
                    input.display(),
                    output.display()
                )
            })?;
            output
        }
        None => input,
    };

    let target_size = args
        .size_mib
        .checked_mul(1024 * 1024)
        .ok_or_else(|| anyhow::anyhow!("image size is too large: {} MiB", args.size_mib))?;
    let current_size = fs::metadata(&image)
        .map_err(|err| anyhow::anyhow!("failed to stat {}: {err}", image.display()))?
        .len();
    if target_size < current_size {
        anyhow::bail!(
            "refusing to shrink {} from {} bytes to {} bytes",
            image.display(),
            current_size,
            target_size
        );
    }

    fs::OpenOptions::new()
        .write(true)
        .open(&image)
        .and_then(|file| file.set_len(target_size))
        .map_err(|err| anyhow::anyhow!("failed to resize {}: {err}", image.display()))?;

    let e2fsck = find_host_tool(
        "E2FSCK",
        "e2fsck",
        &[
            "/opt/homebrew/opt/e2fsprogs/sbin/e2fsck",
            "/usr/local/opt/e2fsprogs/sbin/e2fsck",
        ],
    )?;
    let resize2fs = find_host_tool(
        "RESIZE2FS",
        "resize2fs",
        &[
            "/opt/homebrew/opt/e2fsprogs/sbin/resize2fs",
            "/usr/local/opt/e2fsprogs/sbin/resize2fs",
        ],
    )?;

    let fsck_status = std::process::Command::new(&e2fsck)
        .arg("-fy")
        .arg(&image)
        .status()
        .map_err(|err| anyhow::anyhow!("failed to run {}: {err}", e2fsck.display()))?;
    if !matches!(fsck_status.code(), Some(0 | 1)) {
        anyhow::bail!("{} -fy failed with {fsck_status}", e2fsck.display());
    }

    let resize_status = std::process::Command::new(&resize2fs)
        .arg(&image)
        .status()
        .map_err(|err| anyhow::anyhow!("failed to run {}: {err}", resize2fs.display()))?;
    if !resize_status.success() {
        anyhow::bail!("{} failed with {resize_status}", resize2fs.display());
    }

    println!("image resized at {}", image.display());
    Ok(())
}

fn find_host_tool(env_name: &str, tool_name: &str, fallbacks: &[&str]) -> anyhow::Result<PathBuf> {
    if let Some(configured) = std::env::var_os(env_name).filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(configured));
    }
    if let Some(tool) = find_in_path(tool_name) {
        return Ok(tool);
    }
    for fallback in fallbacks {
        let path = PathBuf::from(fallback);
        if path.is_file() {
            return Ok(path);
        }
    }
    anyhow::bail!(
        "{} not found; install it or set {}=/path/to/{}",
        tool_name,
        env_name,
        tool_name
    )
}

fn find_in_path(tool_name: &str) -> Option<PathBuf> {
    let path = Path::new(tool_name);
    if path.components().count() > 1 && path.is_file() {
        return Some(path.to_path_buf());
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(tool_name))
            .find(|candidate| candidate.is_file())
    })
}

fn to_absolute_path(path: &Path) -> anyhow::Result<PathBuf> {
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    })
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        command: Command,
    }

    #[test]
    fn parses_pull_by_image_name() {
        let cli = Cli::try_parse_from(["image", "pull", "rootfs-riscv64-alpine.img"]).unwrap();

        match cli.command {
            Command::Pull(args) => {
                assert_eq!(args.image.as_deref(), Some("rootfs-riscv64-alpine.img"));
                assert!(args.arch.is_none());
                assert!(args.output_dir.is_none());
                assert!(!args.no_extract);
            }
            _ => panic!("expected pull command"),
        }
    }

    #[test]
    fn parses_pull_by_arch() {
        let cli = Cli::try_parse_from(["image", "pull", "--arch", "x86_64"]).unwrap();

        match cli.command {
            Command::Pull(args) => {
                assert!(args.image.is_none());
                assert_eq!(args.arch.as_deref(), Some("x86_64"));
            }
            _ => panic!("expected pull command"),
        }
    }

    #[test]
    fn parses_pull_with_output_dir_and_no_extract() {
        let cli = Cli::try_parse_from([
            "image",
            "pull",
            "qemu_x86_64_nimbos",
            "--output-dir",
            "tmp/images",
            "--no-extract",
        ])
        .unwrap();

        match cli.command {
            Command::Pull(args) => {
                assert_eq!(args.image.as_deref(), Some("qemu_x86_64_nimbos"));
                assert_eq!(args.output_dir, Some(PathBuf::from("tmp/images")));
                assert!(args.no_extract);
            }
            _ => panic!("expected pull command"),
        }
    }

    #[test]
    fn parses_check_with_expected_sha256() {
        let cli = Cli::try_parse_from([
            "image",
            "check",
            ".tgos-images/rootfs-riscv64-alpine.img/rootfs-riscv64-alpine.img",
            "--sha256",
            "abc",
        ])
        .unwrap();

        match cli.command {
            Command::Check(args) => {
                assert_eq!(
                    args.image,
                    PathBuf::from(
                        ".tgos-images/rootfs-riscv64-alpine.img/rootfs-riscv64-alpine.img"
                    )
                );
                assert_eq!(args.sha256.as_deref(), Some("abc"));
            }
            _ => panic!("expected check command"),
        }
    }

    #[test]
    fn parses_resize_with_output() {
        let cli = Cli::try_parse_from([
            "image",
            "resize",
            "rootfs.img",
            "--size-mib",
            "16384",
            "--output",
            "selfbuild.img",
        ])
        .unwrap();

        match cli.command {
            Command::Resize(args) => {
                assert_eq!(args.image, PathBuf::from("rootfs.img"));
                assert_eq!(args.output, Some(PathBuf::from("selfbuild.img")));
                assert_eq!(args.size_mib, 16384);
            }
            _ => panic!("expected resize command"),
        }
    }
}
