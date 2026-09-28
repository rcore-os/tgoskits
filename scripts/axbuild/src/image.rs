use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, Stdio},
};

use anyhow::{Context, ensure};
use clap::{Args as ClapArgs, Subcommand};

use crate::{
    context::AppContext,
    rootfs::resize::{ResizeOptions, resize_ext_rootfs_image},
    support::download::file_sha256,
};

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
    #[arg(short('R'), long, global = true)]
    pub registry: Option<String>,

    #[arg(short('D'), long, global = true)]
    pub download_dir: Option<PathBuf>,

    #[arg(short('E'), long, global = true)]
    pub extract_dir: Option<PathBuf>,
}

impl ConfigOverrides {
    pub fn apply_on(&self, workspace_root: &Path, config: &mut ImageConfig) {
        if let Some(registry) = self.registry.as_ref() {
            config.registry = registry.clone();
        }
        if let Some(download_dir) = self.download_dir.as_ref() {
            config.download_dir = workspace_relative_path(workspace_root, download_dir);
        }
        if let Some(extract_dir) = self.extract_dir.as_ref() {
            config.extract_dir = workspace_relative_path(workspace_root, extract_dir);
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
    /// Build a reproducible host newc archive from a directory.
    PackInitramfs(ArgsPackInitramfs),
}

#[derive(ClapArgs)]
pub struct ArgsPackInitramfs {
    /// Directory whose contents become the memory root.
    pub source: PathBuf,
    /// Destination archive, outside SOURCE.
    pub output: PathBuf,
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
        Command::Ls(ls) => {
            list_images(app.workspace_root(), app.target_dir(), &args.overrides, ls).await
        }
        Command::Pull(pull) => {
            pull_image(
                app.workspace_root(),
                app.target_dir(),
                &args.overrides,
                pull,
            )
            .await
        }
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
        Command::PackInitramfs(pack) => pack_initramfs(pack),
    }
}

fn pack_initramfs(args: ArgsPackInitramfs) -> anyhow::Result<()> {
    pack_initramfs_dir(&args.source, &args.output)
}

pub(crate) fn pack_initramfs_dir(source: &Path, output: &Path) -> anyhow::Result<()> {
    let source = fs::canonicalize(source)
        .with_context(|| format!("cannot open initramfs source {}", source.display()))?;
    ensure!(source.is_dir(), "initramfs source must be a directory");
    let output = to_absolute_path(output)?;
    let parent = output.parent().context("initramfs output has no parent")?;
    fs::create_dir_all(parent)?;
    let parent = fs::canonicalize(parent)?;
    let output = parent.join(
        output
            .file_name()
            .context("initramfs output has no filename")?,
    );
    ensure!(
        !output.starts_with(&source),
        "initramfs output must be outside its source directory"
    );
    let paths = archive_paths(&source)?;
    let temp = tempfile::NamedTempFile::new_in(&parent)?;
    let mut child = ProcessCommand::new("cpio")
        .args([
            "--create",
            "--null",
            "--format=newc",
            "--reproducible",
            "--owner=0:0",
            "--quiet",
        ])
        .current_dir(&source)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(temp.reopen()?))
        .spawn()
        .context("failed to start cpio; install GNU cpio")?;
    let write_result = (|| -> anyhow::Result<()> {
        let mut input = child.stdin.take().context("cpio stdin is unavailable")?;
        for path in &paths {
            let path = path.to_str().context("initramfs path is not UTF-8")?;
            ensure!(!path.contains('\0'), "initramfs path contains NUL");
            input.write_all(path.as_bytes())?;
            input.write_all(&[0])?;
        }
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    ensure!(
        child.wait()?.success(),
        "cpio failed to create host initramfs"
    );
    ensure!(
        temp.as_file().metadata()?.len() != 0,
        "cpio created an empty archive"
    );
    temp.persist(&output)?;
    println!("host initramfs: {}", output.display());
    Ok(())
}

fn archive_paths(source: &Path) -> anyhow::Result<Vec<PathBuf>> {
    fn visit(source: &Path, relative: &Path, out: &mut Vec<PathBuf>) -> anyhow::Result<()> {
        let mut entries =
            fs::read_dir(source.join(relative))?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = relative.join(entry.file_name());
            let metadata = fs::symlink_metadata(entry.path())?;
            out.push(path.clone());
            if metadata.is_dir() {
                visit(source, &path, out)?;
            }
        }
        Ok(())
    }
    let mut paths = vec![PathBuf::from(".")];
    visit(source, Path::new("."), &mut paths)?;
    Ok(paths)
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
    target_dir: &Path,
    overrides: &ConfigOverrides,
    args: ArgsLs,
) -> anyhow::Result<()> {
    let mut config = ImageConfig::read_config(workspace_root, target_dir)?;
    overrides.apply_on(workspace_root, &mut config);
    let storage = Storage::new_from_config(&config).await?;
    storage
        .image_registry
        .print(args.verbose, args.pattern.as_deref());
    Ok(())
}

async fn pull_image(
    workspace_root: &Path,
    target_dir: &Path,
    overrides: &ConfigOverrides,
    args: ArgsPull,
) -> anyhow::Result<()> {
    let image_path = match (args.image.as_deref(), args.arch.as_deref()) {
        (Some(image), None) if !args.no_extract => {
            let mut config = ImageConfig::read_config(workspace_root, target_dir)?;
            overrides.apply_on(workspace_root, &mut config);
            let storage = Storage::new_from_config(&config).await?;
            match storage.pull_rootfs_image(ImageSpecRef::parse(image)).await {
                Ok(path) => path,
                Err(rootfs_err) => storage
                    .pull_image(ImageSpecRef::parse(image), true)
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
            let mut config = ImageConfig::read_config(workspace_root, target_dir)?;
            overrides.apply_on(workspace_root, &mut config);
            let storage = Storage::new_from_config(&config).await?;
            storage
                .pull_image(ImageSpecRef::parse(image), !args.no_extract)
                .await?
        }
        (None, Some(arch)) if !args.no_extract => {
            let mut config = ImageConfig::read_config(workspace_root, target_dir)?;
            overrides.apply_on(workspace_root, &mut config);
            let image = storage::default_rootfs_image(arch).ok_or_else(|| {
                anyhow::anyhow!("no managed rootfs image available for arch `{arch}`")
            })?;
            let storage = Storage::new_from_config(&config).await?;
            storage.pull_rootfs_image(image.into()).await?
        }
        (None, Some(_)) => {
            anyhow::bail!("`--arch` managed rootfs pulls do not accept `--no-extract`")
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
    let output = args.output.as_deref().map(to_absolute_path).transpose()?;
    let image = resize_ext_rootfs_image(ResizeOptions {
        input,
        output,
        size_mib: args.size_mib,
    })?;

    println!("rootfs image resized at {}", image.display());
    Ok(())
}

fn to_absolute_path(path: &Path) -> anyhow::Result<PathBuf> {
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    })
}

fn workspace_relative_path(workspace_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn initramfs_paths_are_sorted_and_include_root() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join("etc")).unwrap();
        fs::write(root.path().join("z"), b"z").unwrap();
        fs::write(root.path().join("etc/issue"), b"hello").unwrap();
        assert_eq!(
            archive_paths(root.path()).unwrap(),
            [
                PathBuf::from("."),
                PathBuf::from("./etc"),
                PathBuf::from("./etc/issue"),
                PathBuf::from("./z"),
            ]
        );
    }

    #[test]
    fn cli_paths_override_environment_config_relative_to_workspace() {
        let workspace = tempdir().unwrap();
        let mut config = ImageConfig {
            registry: "configured".to_string(),
            download_dir: workspace.path().join("env-downloads"),
            extract_dir: workspace.path().join("env-images"),
        };
        ConfigOverrides {
            registry: Some("cli".to_string()),
            download_dir: Some(PathBuf::from("cli-downloads")),
            extract_dir: Some(PathBuf::from("cli-images")),
        }
        .apply_on(workspace.path(), &mut config);

        assert_eq!(config.registry, "cli");
        assert_eq!(config.download_dir, workspace.path().join("cli-downloads"));
        assert_eq!(config.extract_dir, workspace.path().join("cli-images"));
    }
}
