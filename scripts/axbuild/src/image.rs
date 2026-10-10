use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, ensure};
use clap::{Args as ClapArgs, Subcommand};
use flate2::{Compression, write::GzEncoder};

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
    /// Compress the newc stream with gzip.
    #[arg(long)]
    pub gzip: bool,
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
    pack_initramfs_dir_with_compression(
        &args.source,
        &args.output,
        if args.gzip {
            InitramfsCompression::Gzip(Compression::default())
        } else {
            InitramfsCompression::None
        },
    )
}

pub(crate) fn pack_initramfs_dir(source: &Path, output: &Path) -> anyhow::Result<()> {
    pack_initramfs_dir_with_compression(source, output, InitramfsCompression::None)
}

fn pack_initramfs_dir_with_compression(
    source: &Path,
    output: &Path,
    compression: InitramfsCompression,
) -> anyhow::Result<()> {
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
    let builder = InitramfsBuilder::from_directory(&source)?;
    builder.write_to_path(&output, compression)?;
    println!("host initramfs: {}", output.display());
    Ok(())
}

/// Compression applied to a generated initramfs archive.
#[derive(Clone, Copy, Debug)]
pub(crate) enum InitramfsCompression {
    None,
    Gzip(Compression),
}

#[derive(Debug)]
enum InitramfsEntry {
    Directory { mode: u32 },
    File { mode: u32, contents: Vec<u8> },
    Symlink { mode: u32, target: Vec<u8> },
    Trailer,
}

/// Deterministic newc archive builder shared by all host-side image staging.
///
/// Guest paths are relative to the archive root. Every entry is owned by
/// root, has a zero mtime, and is emitted in lexical order. The builder keeps
/// entries in memory so callers can compose archives without a temporary
/// staging directory and so duplicate paths are rejected at insertion time.
#[derive(Debug, Default)]
pub(crate) struct InitramfsBuilder {
    entries: BTreeMap<String, InitramfsEntry>,
}

impl InitramfsBuilder {
    pub(crate) fn new() -> Self {
        let mut builder = Self::default();
        builder
            .entries
            .insert(".".to_string(), InitramfsEntry::Directory { mode: 0o755 });
        builder
    }

    pub(crate) fn from_directory(source: &Path) -> anyhow::Result<Self> {
        ensure!(source.is_dir(), "initramfs source must be a directory");
        let mut builder = Self::new();
        builder.add_directory_tree(source, Path::new("."))?;
        Ok(builder)
    }

    pub(crate) fn add_directory(&mut self, path: &str, mode: u32) -> anyhow::Result<()> {
        let path = normalize_archive_path(path)?;
        self.insert(
            path,
            InitramfsEntry::Directory {
                mode: mode & 0o7777,
            },
        )
    }

    pub(crate) fn add_file(
        &mut self,
        path: &str,
        contents: &[u8],
        mode: u32,
    ) -> anyhow::Result<()> {
        let path = normalize_archive_path(path)?;
        self.insert(
            path,
            InitramfsEntry::File {
                mode: mode & 0o7777,
                contents: contents.to_vec(),
            },
        )
    }

    pub(crate) fn add_symlink(
        &mut self,
        path: &str,
        target: &str,
        mode: u32,
    ) -> anyhow::Result<()> {
        let path = normalize_archive_path(path)?;
        ensure!(
            !target.as_bytes().contains(&0),
            "initramfs symlink target contains NUL"
        );
        self.insert(
            path,
            InitramfsEntry::Symlink {
                mode: mode & 0o7777,
                target: target.as_bytes().to_vec(),
            },
        )
    }

    pub(crate) fn add_directory_tree(
        &mut self,
        source: &Path,
        relative: &Path,
    ) -> anyhow::Result<()> {
        let mut entries =
            fs::read_dir(source.join(relative))?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let child_relative = relative.join(entry.file_name());
            let archive_path = child_relative
                .to_str()
                .context("initramfs path is not UTF-8")?;
            let metadata = fs::symlink_metadata(entry.path())?;
            #[cfg(unix)]
            let mode = std::os::unix::fs::MetadataExt::mode(&metadata);
            #[cfg(not(unix))]
            let mode = if metadata.is_dir() { 0o755 } else { 0o644 };
            if metadata.is_dir() {
                self.add_directory(archive_path, mode)?;
                self.add_directory_tree(source, &child_relative)?;
            } else if metadata.file_type().is_symlink() {
                let target = fs::read_link(entry.path())?;
                let target = target
                    .to_str()
                    .context("initramfs symlink target is not UTF-8")?;
                self.add_symlink(archive_path, target, mode)?;
            } else if metadata.is_file() {
                let contents = fs::read(entry.path())?;
                self.add_file(archive_path, &contents, mode)?;
            } else {
                anyhow::bail!("unsupported initramfs entry {}", entry.path().display());
            }
        }
        Ok(())
    }

    pub(crate) fn build(&self, compression: InitramfsCompression) -> anyhow::Result<Vec<u8>> {
        let mut archive = Vec::new();
        let mut inode = 1u32;
        for (path, entry) in &self.entries {
            append_newc_entry(&mut archive, &mut inode, path, entry)?;
        }
        append_newc_entry(
            &mut archive,
            &mut inode,
            "TRAILER!!!",
            &InitramfsEntry::Trailer,
        )?;
        match compression {
            InitramfsCompression::None => Ok(archive),
            InitramfsCompression::Gzip(level) => {
                let mut encoder = GzEncoder::new(Vec::new(), level);
                encoder.write_all(&archive)?;
                encoder
                    .finish()
                    .context("failed to finish initramfs gzip stream")
            }
        }
    }

    pub(crate) fn write_to_path(
        &self,
        output: &Path,
        compression: InitramfsCompression,
    ) -> anyhow::Result<()> {
        let parent = output.parent().context("initramfs output has no parent")?;
        fs::create_dir_all(parent)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(&self.build(compression)?)?;
        temporary.as_file_mut().sync_all()?;
        temporary.persist(output).map_err(|error| error.error)?;
        Ok(())
    }

    fn insert(&mut self, path: String, entry: InitramfsEntry) -> anyhow::Result<()> {
        ensure!(
            !self.entries.contains_key(&path),
            "duplicate initramfs path `{path}`"
        );
        if path != "." {
            let mut parent_path = Path::new(&path).parent();
            while let Some(current_parent) = parent_path {
                let parent = if current_parent.as_os_str().is_empty() {
                    "."
                } else {
                    current_parent
                        .to_str()
                        .context("initramfs path is not UTF-8")?
                };
                if let Some(InitramfsEntry::File { .. } | InitramfsEntry::Symlink { .. }) =
                    self.entries.get(parent)
                {
                    anyhow::bail!("initramfs parent `{parent}` is not a directory");
                }
                if parent == "." {
                    break;
                }
                parent_path = current_parent.parent();
            }
        }
        self.entries.insert(path, entry);
        Ok(())
    }
}

fn normalize_archive_path(path: &str) -> anyhow::Result<String> {
    ensure!(
        !path.is_empty() && !path.as_bytes().contains(&0),
        "invalid initramfs path"
    );
    let mut components = Vec::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(component) => {
                let component = component.to_str().context("initramfs path is not UTF-8")?;
                ensure!(!component.is_empty(), "invalid initramfs path");
                components.push(component);
            }
            Component::CurDir => {}
            Component::RootDir | Component::ParentDir | Component::Prefix(_) => {
                anyhow::bail!("initramfs path escapes archive root: `{path}`")
            }
        }
    }
    ensure!(!components.is_empty(), "invalid initramfs path");
    Ok(components.join("/"))
}

fn append_newc_entry(
    archive: &mut Vec<u8>,
    inode: &mut u32,
    path: &str,
    entry: &InitramfsEntry,
) -> anyhow::Result<()> {
    let (mode_type, mode, nlink, contents) = match entry {
        InitramfsEntry::Directory { mode } => (0o040000, *mode, 2, Vec::new()),
        InitramfsEntry::File { mode, contents } => (0o100000, *mode, 1, contents.clone()),
        InitramfsEntry::Symlink { mode, target } => (0o120000, *mode, 1, target.clone()),
        InitramfsEntry::Trailer => (0, 0, 1, Vec::new()),
    };
    let full_mode = mode_type | mode;
    let size = u32::try_from(contents.len()).context("initramfs entry is larger than 4 GiB")?;
    let name_size = u32::try_from(path.len() + 1).context("initramfs path is too long")?;
    let mut header = [0u8; 110];
    write!(&mut header[..], "070701").unwrap();
    let fields = [
        *inode, full_mode, 0, 0, nlink, 0, size, 0, 0, 0, 0, name_size, 0,
    ];
    for (index, field) in fields.into_iter().enumerate() {
        let start = 6 + index * 8;
        write!(&mut header[start..start + 8], "{field:08x}").unwrap();
    }
    archive.extend_from_slice(&header);
    archive.extend_from_slice(path.as_bytes());
    archive.push(0);
    pad_vec(archive, 110 + path.len() + 1);
    archive.extend_from_slice(&contents);
    pad_vec(archive, contents.len());
    *inode = inode.checked_add(1).context("initramfs inode overflow")?;
    Ok(())
}

fn pad_vec(output: &mut Vec<u8>, written: usize) {
    let padding = (4 - written % 4) % 4;
    output.resize(output.len() + padding, 0);
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
    use std::time::{Duration, UNIX_EPOCH};

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn initramfs_builder_sorts_paths_and_includes_root() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join("etc")).unwrap();
        fs::write(root.path().join("z"), b"z").unwrap();
        fs::write(root.path().join("etc/issue"), b"hello").unwrap();
        let archive = InitramfsBuilder::from_directory(root.path())
            .unwrap()
            .build(InitramfsCompression::None)
            .unwrap();
        let entries = parse_newc_entries(&archive);
        assert_eq!(
            entries.keys().collect::<Vec<_>>(),
            [
                &".".to_string(),
                &"etc".to_string(),
                &"etc/issue".to_string(),
                &"z".to_string()
            ]
        );
        assert_eq!(entries["etc/issue"], b"hello");
    }

    #[test]
    fn packed_initramfs_ignores_source_mtime() {
        let root = tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        let entry = source.join("init");
        fs::write(&entry, b"init").unwrap();

        let set_mtime = |seconds| {
            let time = fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(seconds));
            fs::File::open(&entry).unwrap().set_times(time).unwrap();
            fs::File::open(&source).unwrap().set_times(time).unwrap();
        };
        set_mtime(1_000_000);
        let first = root.path().join("first.cpio");
        pack_initramfs_dir(&source, &first).unwrap();

        set_mtime(2_000_000);
        let second = root.path().join("second.cpio");
        pack_initramfs_dir(&source, &second).unwrap();

        assert!(fs::read(&first).unwrap() == fs::read(&second).unwrap());
        let entries = parse_newc_entries(&fs::read(second).unwrap());
        assert_eq!(entries["init"], b"init");
    }

    #[test]
    fn initramfs_builder_rejects_escape_and_duplicates() {
        let mut builder = InitramfsBuilder::new();
        assert!(builder.add_file("../escape", b"x", 0o644).is_err());
        builder.add_file("x", b"x", 0o644).unwrap();
        assert!(builder.add_file("x", b"y", 0o644).is_err());
        assert!(builder.add_symlink("bad", "a\0b", 0o777).is_err());
    }

    #[test]
    fn initramfs_builder_supports_gzip_and_symlinks() {
        let mut builder = InitramfsBuilder::new();
        builder.add_directory("bin", 0o755).unwrap();
        builder.add_file("bin/sh", b"shell", 0o755).unwrap();
        builder.add_symlink("sh", "bin/sh", 0o777).unwrap();
        let compressed = builder
            .build(InitramfsCompression::Gzip(Compression::fast()))
            .unwrap();
        let mut archive = Vec::new();
        use std::io::Read;
        flate2::read::GzDecoder::new(compressed.as_slice())
            .read_to_end(&mut archive)
            .unwrap();
        let entries = parse_newc_entries(&archive);
        assert_eq!(entries["bin/sh"], b"shell");
        assert_eq!(entries["sh"], b"bin/sh");
    }

    fn parse_newc_entries(archive: &[u8]) -> BTreeMap<String, Vec<u8>> {
        let mut entries = BTreeMap::new();
        let mut offset = 0usize;
        loop {
            let header = &archive[offset..offset + 110];
            assert_eq!(&header[..6], b"070701");
            let field = |index: usize| {
                usize::from_str_radix(
                    std::str::from_utf8(&header[6 + index * 8..14 + index * 8]).unwrap(),
                    16,
                )
                .unwrap()
            };
            let size = field(6);
            let name_size = field(11);
            let name_start = offset + 110;
            let name_end = name_start + name_size;
            let name = std::str::from_utf8(&archive[name_start..name_end - 1])
                .unwrap()
                .to_string();
            let data_start = (name_end + 3) & !3;
            let data_end = data_start + size;
            if name == "TRAILER!!!" {
                break;
            }
            entries.insert(name, archive[data_start..data_end].to_vec());
            offset = (data_end + 3) & !3;
        }
        entries
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
