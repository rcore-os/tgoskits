mod cargo;
mod clippy;
mod cmd;
mod ctx;
mod devspace;
mod image;
mod menuconfig;
mod tbuld;
mod vmconfig;

pub mod xtest;

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, anyhow};
use chrono::Utc;
pub use cmd::*;

pub async fn run(command: Commands, repo_root: impl AsRef<Path>) -> Result<()> {
    let repo_root = repo_root.as_ref();
    let mut ctx = ctx::Context::new(repo_root);

    match command {
        Commands::Defconfig { board_name } => {
            defconfig_command(repo_root, &board_name)?;
        }
        Commands::Build(args) => {
            println!("Building the project...");
            ctx.apply_build_args(&args);
            ctx.run_build().await?;
            println!("Build completed successfully.");
        }
        Commands::Clippy(args) => {
            clippy::run_clippy(args)?;
        }
        Commands::Qemu(args) => {
            ctx.apply_build_args(&args.build);
            ctx.vmconfigs = args.vmconfigs;
            ctx.build_config_path = args
                .build_config
                .map(|path| resolve_repo_path(repo_root, path));
            ctx.run_qemu(args.qemu_config).await?;
        }
        Commands::Uboot(args) => {
            ctx.apply_build_args(&args.build);
            ctx.vmconfigs = args.vmconfigs;
            ctx.build_config_path = args
                .build_config
                .map(|path| resolve_repo_path(repo_root, path));
            ctx.run_uboot(args.uboot_config).await?;
        }
        Commands::Vmconfig => {
            ctx.run_vmconfig().await?;
        }
        Commands::Menuconfig => {
            ctx.run_menuconfig().await?;
        }
        Commands::Image(args) => {
            image::run_image(args, repo_root).await?;
        }
        Commands::Devspace(args) => match args.action {
            DevspaceCommand::Start => devspace::start(repo_root)?,
            DevspaceCommand::Stop => devspace::stop(repo_root)?,
        },
    }

    Ok(())
}

pub fn resolve_repo_path(repo_root: &Path, path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        repo_root.join(path)
    }
}

pub(crate) fn build_config_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".build.toml")
}

pub(crate) fn build_schema_path(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".build-schema.json")
}

pub(crate) fn vmconfig_schema_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".vmconfig-schema.json")
}

pub(crate) fn cargo_manifest_path(repo_root: &Path) -> PathBuf {
    repo_root.join("Cargo.toml")
}

pub(crate) fn board_config_path(repo_root: &Path, board_name: &str) -> PathBuf {
    repo_root
        .join("configs")
        .join("board")
        .join(format!("{board_name}.toml"))
}

pub(crate) fn default_qemu_config_path(repo_root: &Path, arch: &str) -> PathBuf {
    repo_root
        .join("scripts")
        .join("ostool")
        .join(format!("qemu-{arch}.toml"))
}

fn defconfig_command(repo_root: &Path, board_name: &str) -> Result<()> {
    println!("Setting default configuration for board: {board_name}");

    let board_config_path = board_config_path(repo_root, board_name);
    if !board_config_path.exists() {
        return Err(anyhow!(
            "Board configuration '{board_name}' not found. Available boards: {}",
            available_boards(repo_root)?.join(", ")
        ));
    }

    backup_existing_config(repo_root)?;
    copy_board_config(repo_root, board_name)?;

    println!("Successfully set default configuration to: {board_name}");
    Ok(())
}

fn backup_existing_config(repo_root: &Path) -> Result<()> {
    let build_config_path = build_config_path(repo_root);
    if build_config_path.exists() {
        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        let backup_path = repo_root.join(format!(".build.toml.backup_{timestamp}"));

        fs::copy(&build_config_path, &backup_path).with_context(|| {
            format!(
                "Failed to backup {} to {}",
                build_config_path.display(),
                backup_path.display()
            )
        })?;

        println!(
            "Backed up existing configuration to: {}",
            backup_path.display()
        );
    }

    Ok(())
}

fn copy_board_config(repo_root: &Path, board_name: &str) -> Result<()> {
    let source_path = board_config_path(repo_root, board_name);
    let target_path = build_config_path(repo_root);

    fs::copy(&source_path, &target_path).with_context(|| {
        format!(
            "Failed to copy {} to {}",
            source_path.display(),
            target_path.display()
        )
    })?;

    println!("Copied board configuration from: {}", source_path.display());
    Ok(())
}

fn available_boards(repo_root: &Path) -> Result<Vec<String>> {
    let board_dir = repo_root.join("configs").join("board");
    let mut boards = fs::read_dir(&board_dir)
        .with_context(|| {
            format!(
                "Failed to read board config directory {}",
                board_dir.display()
            )
        })?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .filter_map(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .collect::<Vec<_>>();
    boards.sort();
    Ok(boards)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use tempfile::tempdir;

    use super::{Commands, board_config_path, build_config_path, default_qemu_config_path, run};

    #[test]
    fn helper_paths_are_repo_root_relative() {
        let repo_root = Path::new("/tmp/axvisor");

        assert_eq!(
            build_config_path(repo_root),
            Path::new("/tmp/axvisor/.build.toml")
        );
        assert_eq!(
            board_config_path(repo_root, "qemu-aarch64"),
            Path::new("/tmp/axvisor/configs/board/qemu-aarch64.toml")
        );
        assert_eq!(
            default_qemu_config_path(repo_root, "x86_64"),
            Path::new("/tmp/axvisor/scripts/ostool/qemu-x86_64.toml")
        );
        assert_eq!(
            default_qemu_config_path(repo_root, "aarch64"),
            Path::new("/tmp/axvisor/scripts/ostool/qemu-aarch64.toml")
        );
        assert_eq!(
            default_qemu_config_path(repo_root, "riscv64"),
            Path::new("/tmp/axvisor/scripts/ostool/qemu-riscv64.toml")
        );
    }

    #[test]
    fn defconfig_creates_backup_under_repo_root() {
        let tempdir = tempdir().expect("tempdir should be created");
        let repo_root = tempdir.path();
        let board_dir = repo_root.join("configs").join("board");
        fs::create_dir_all(&board_dir).expect("board directory should be created");
        fs::write(board_dir.join("qemu-aarch64.toml"), "target = 'test'\n")
            .expect("board config should be written");
        fs::write(repo_root.join(".build.toml"), "old = true\n")
            .expect("existing build config should be written");

        let runtime = tokio::runtime::Runtime::new().expect("runtime should be created");
        runtime
            .block_on(run(
                Commands::Defconfig {
                    board_name: "qemu-aarch64".to_string(),
                },
                repo_root,
            ))
            .expect("defconfig should succeed");

        let build_contents =
            fs::read_to_string(repo_root.join(".build.toml")).expect("build config should exist");
        assert_eq!(build_contents, "target = 'test'\n");

        let backups = fs::read_dir(repo_root)
            .expect("repo root should be readable")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".build.toml.backup_"))
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1);
    }
}
