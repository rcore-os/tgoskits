use std::path::{Path, PathBuf};

use anyhow::Context;
use ax_config_gen::{
    GenerateOptions, OutputFormat, generate_config, load_config_specs, read_config_value,
    read_loaded_config_string, read_loaded_config_value,
};
use clap::{Args, Subcommand};

use crate::build::{
    resolve_platform_config_by_package, resolve_platform_config_by_package_with_metadata,
    workspace_metadata,
};

#[derive(Subcommand)]
pub enum Command {
    /// Locate a platform package axconfig.toml
    PlatformPath(PlatformPathArgs),
    /// Read a config item from merged config specs
    Read(ReadArgs),
    /// Generate a merged config file
    Generate(GenerateArgs),
    /// Inspect platform config fields used by the ArceOS Makefile
    Inspect(InspectArgs),
}

#[derive(Args)]
pub struct PlatformPathArgs {
    /// Platform package name
    #[arg(long = "package", value_name = "PACKAGE")]
    package: String,
}

#[derive(Args)]
pub struct ReadArgs {
    /// Config specification files merged in order
    #[arg(required = true, value_name = "SPEC")]
    specs: Vec<PathBuf>,
    /// Config item to read, in key or table.key form
    #[arg(short, long, value_name = "ITEM")]
    read: String,
}

#[derive(Args)]
pub struct GenerateArgs {
    /// Config specification files merged in order
    #[arg(required = true, value_name = "SPEC")]
    specs: Vec<PathBuf>,
    /// Path to the old config file
    #[arg(short = 'c', long)]
    oldconfig: Option<PathBuf>,
    /// Path to the output config file
    #[arg(short, long)]
    output: PathBuf,
    /// Setting a config item with format table.key=value
    #[arg(short, long, value_name = "WR_CONFIG")]
    write: Vec<String>,
}

#[derive(Args)]
pub struct InspectArgs {
    /// Platform package name
    #[arg(long = "package", value_name = "PACKAGE")]
    package: String,
    /// Directory containing the application Cargo.toml used for dependency lookup
    #[arg(long = "manifest-dir", value_name = "DIR")]
    manifest_dir: Option<PathBuf>,
    /// Optional explicit platform config path
    #[arg(long = "config", value_name = "PATH")]
    config: Option<PathBuf>,
    /// Print a single-line key=value form for Makefile parsing
    #[arg(long)]
    makefile: bool,
}

pub fn execute(command: Command) -> anyhow::Result<()> {
    match command {
        Command::PlatformPath(args) => platform_path(args),
        Command::Read(args) => read(args),
        Command::Generate(args) => generate(args),
        Command::Inspect(args) => inspect(args),
    }
}

fn platform_path(args: PlatformPathArgs) -> anyhow::Result<()> {
    let metadata = workspace_metadata().context("failed to load workspace metadata")?;
    let platform = resolve_platform_config_by_package(&args.package, &metadata)?;
    println!("{}", platform.config_path.display());
    Ok(())
}

fn read(args: ReadArgs) -> anyhow::Result<()> {
    let value = read_config_value(&args.specs, &args.read).with_context(|| {
        format!(
            "failed to read config item `{}` from {}",
            args.read,
            args.specs
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;
    println!("{value}");
    Ok(())
}

fn generate(args: GenerateArgs) -> anyhow::Result<()> {
    let report = generate_config(&GenerateOptions {
        specs: args.specs,
        oldconfig: args.oldconfig,
        output: Some(args.output),
        fmt: OutputFormat::Toml,
        writes: args.write,
        keep_backup: true,
    })
    .context("failed to generate config")?;

    for item in report.untouched {
        eprintln!(
            "[WARN] config item `{}` not set in the old config, using default value",
            item.item_name()
        );
    }
    for item in report.extra {
        eprintln!(
            "[WARN] config item `{}` not found in the specification, ignoring",
            item.item_name()
        );
    }

    Ok(())
}

fn inspect(args: InspectArgs) -> anyhow::Result<()> {
    let platform_config = match args.config {
        Some(path) => path,
        None => {
            if let Some(manifest_dir) = args.manifest_dir.as_deref() {
                resolve_platform_config_from_manifest_dir(&args.package, manifest_dir)?.config_path
            } else {
                let metadata = workspace_metadata().context("failed to load workspace metadata")?;
                resolve_platform_config_by_package(&args.package, &metadata)?.config_path
            }
        }
    };
    let config = load_config_specs(std::slice::from_ref(&platform_config)).with_context(|| {
        format!(
            "failed to load platform config {}",
            platform_config.display()
        )
    })?;

    let package =
        read_loaded_config_string(&config, "package").context("failed to read package")?;
    let platform =
        read_loaded_config_string(&config, "platform").context("failed to read platform")?;
    let arch = read_loaded_config_string(&config, "arch").context("failed to read arch")?;
    let max_cpu_num =
        read_loaded_config_value(&config, "plat.max-cpu-num").unwrap_or_else(|_| String::new());
    let phys_memory_size = read_loaded_config_value(&config, "plat.phys-memory-size")
        .unwrap_or_else(|_| String::new());

    let platform_config = platform_config.display().to_string();
    if args.makefile {
        println!(
            "PLAT_CONFIG={} PLAT_PACKAGE={} PLAT_NAME={} PLAT_ARCH={} PLAT_SMP={} \
             PHYS_MEMORY_SIZE={}",
            platform_config, package, platform, arch, max_cpu_num, phys_memory_size
        );
    } else {
        println!("PLAT_CONFIG={}", shell_escape(&platform_config));
        println!("PLAT_PACKAGE={}", shell_escape(&package));
        println!("PLAT_NAME={}", shell_escape(&platform));
        println!("PLAT_ARCH={}", shell_escape(&arch));
        println!("PLAT_SMP={max_cpu_num}");
        println!("PHYS_MEMORY_SIZE={phys_memory_size}");
    }

    Ok(())
}

fn resolve_platform_config_from_manifest_dir(
    package: &str,
    manifest_dir: &Path,
) -> anyhow::Result<crate::build::ResolvedPlatformConfig> {
    let manifest_path = manifest_dir.join("Cargo.toml");
    let metadata = crate::context::workspace_metadata_root_manifest(&manifest_path)
        .with_context(|| format!("failed to load metadata for {}", manifest_path.display()))?;
    let deps_metadata = crate::context::workspace_metadata_root_manifest_with_deps(&manifest_path)
        .with_context(|| {
            format!(
                "failed to load dependency metadata for {}",
                manifest_path.display()
            )
        })?;
    resolve_platform_config_by_package_with_metadata(package, &metadata, &deps_metadata)
}

fn shell_escape(value: &str) -> String {
    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/'))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}
