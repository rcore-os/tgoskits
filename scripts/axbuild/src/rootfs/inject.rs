//! Rootfs image content extraction and overlay injection helpers.
//!
//! Main responsibilities:
//! - Use `debugfs` to extract a rootfs image into a staging directory
//! - Write overlay files and directories back into a rootfs image
//! - Generate and execute `debugfs` scripts for image content updates
//!
//! Unlike [`super::qemu`], this file operates on the contents of the rootfs
//! image itself.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, bail, ensure};

/// Extracts the contents of a rootfs image into a host staging directory.
pub(crate) fn extract_rootfs(rootfs_img: &Path, output_dir: &Path) -> anyhow::Result<()> {
    Command::new("debugfs")
        .arg("-R")
        .arg(format!("rdump / {}", output_dir.display()))
        .arg(rootfs_img)
        .status()
        .with_context(|| format!("failed to spawn debugfs for {}", rootfs_img.display()))?
        .success()
        .then_some(())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "failed to extract {} into {}",
                rootfs_img.display(),
                output_dir.display()
            )
        })
}

/// Injects an overlay directory tree into an existing rootfs image.
pub(crate) fn inject_overlay(rootfs_img: &Path, overlay_dir: &Path) -> anyhow::Result<()> {
    ensure!(
        overlay_has_entries(overlay_dir)?,
        "overlay injection source is empty: {}",
        overlay_dir.display()
    );

    let mut commands = Vec::new();
    collect_overlay_debugfs_commands(overlay_dir, Path::new(""), &mut commands)?;
    run_debugfs_script(
        rootfs_img,
        &commands,
        &format!(
            "failed to inject overlay {} into {}",
            overlay_dir.display(),
            rootfs_img.display()
        ),
    )
}

/// Returns whether an overlay directory contains at least one entry.
fn overlay_has_entries(overlay_dir: &Path) -> anyhow::Result<bool> {
    Ok(fs::read_dir(overlay_dir)
        .with_context(|| format!("failed to read {}", overlay_dir.display()))?
        .next()
        .is_some())
}

/// Converts an overlay directory tree into a sequence of `debugfs` commands.
fn collect_overlay_debugfs_commands(
    overlay_dir: &Path,
    relative_dir: &Path,
    commands: &mut Vec<String>,
) -> anyhow::Result<()> {
    let current_dir = if relative_dir.as_os_str().is_empty() {
        overlay_dir.to_path_buf()
    } else {
        overlay_dir.join(relative_dir)
    };
    let mut entries = fs::read_dir(&current_dir)
        .with_context(|| format!("failed to read {}", current_dir.display()))?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("failed to read {}", current_dir.display()))?;
    entries.sort_by_key(|left| left.file_name());

    for entry in entries {
        let file_name = PathBuf::from(entry.file_name());
        let relative_path = relative_dir.join(&file_name);
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", entry.path().display()))?;

        if file_type.is_dir() {
            commands.push(format!("mkdir /{}", relative_path.display()));
            collect_overlay_debugfs_commands(overlay_dir, &relative_path, commands)?;
            continue;
        }

        ensure!(
            file_type.is_file(),
            "unsupported overlay entry `{}`; only regular files and directories are supported",
            entry.path().display()
        );
        commands.push(format!("rm /{}", relative_path.display()));
        commands.push(format!(
            "write {} /{}",
            entry.path().display(),
            relative_path.display()
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = fs::metadata(entry.path())
                .with_context(|| format!("failed to stat {}", entry.path().display()))?;
            commands.push(format!(
                "sif /{} mode 0{:o}",
                relative_path.display(),
                metadata.permissions().mode()
            ));
        }
    }

    Ok(())
}

/// Executes a generated `debugfs` script against a writable rootfs image.
fn run_debugfs_script(
    rootfs_img: &Path,
    commands: &[String],
    context_message: &str,
) -> anyhow::Result<()> {
    eprintln!("debugfs -w {}", rootfs_img.display());
    let mut child = Command::new("debugfs")
        .arg("-w")
        .arg(rootfs_img)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("failed to spawn debugfs for {}", rootfs_img.display()))?;

    {
        let mut stdin = child.stdin.take().context("failed to open debugfs stdin")?;
        for command in commands {
            writeln!(stdin, "{command}").context("failed to write debugfs command")?;
        }
        writeln!(stdin, "quit").context("failed to finalize debugfs script")?;
    }

    let status = child.wait().context("failed to wait for debugfs")?;
    if status.success() {
        Ok(())
    } else {
        bail!("{context_message}: debugfs exited with status {status}");
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use tempfile::tempdir;

    use super::*;

    #[cfg(unix)]
    #[test]
    fn overlay_debugfs_commands_include_paths_and_modes() {
        let root = tempdir().unwrap();
        let overlay_dir = root.path().join("overlay");
        fs::create_dir_all(overlay_dir.join("usr/bin")).unwrap();
        let binary = overlay_dir.join("usr/bin/test-bin");
        fs::write(&binary, b"bin").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        }

        let mut commands = Vec::new();
        collect_overlay_debugfs_commands(&overlay_dir, Path::new(""), &mut commands).unwrap();

        assert_eq!(commands[0], "mkdir /usr");
        assert!(commands.contains(&"mkdir /usr/bin".to_string()));
        assert!(commands.contains(&format!("write {} /usr/bin/test-bin", binary.display())));
        assert!(commands.contains(&"sif /usr/bin/test-bin mode 0100755".to_string()));
    }
}
