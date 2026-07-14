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
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
};

use anyhow::{Context, bail, ensure};

/// Reads a text file from a rootfs image with `debugfs`.
///
/// Returns `Ok(None)` when the image is readable but the guest path does not
/// exist, allowing distro-specific files to be optional.
pub(crate) fn read_text_file(
    rootfs_img: &Path,
    guest_path: &str,
) -> anyhow::Result<Option<String>> {
    ensure!(
        guest_path.starts_with('/'),
        "guest path must be absolute: `{guest_path}`"
    );

    let output = Command::new("debugfs")
        .arg("-R")
        .arg(format!("cat {guest_path}"))
        .arg(rootfs_img)
        .output()
        .with_context(|| format!("failed to spawn debugfs for {}", rootfs_img.display()))?;
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        bail!(
            "failed to read {guest_path} from {}: {}",
            rootfs_img.display(),
            stderr.trim()
        );
    }
    if output.stdout.is_empty() && stderr.contains("File not found") {
        return Ok(None);
    }

    String::from_utf8(output.stdout)
        .map(Some)
        .with_context(|| format!("{}:{guest_path} is not valid UTF-8", rootfs_img.display()))
}

/// Replaces one regular file inside a rootfs image with a host file.
pub(crate) fn replace_file(
    rootfs_img: &Path,
    guest_path: &str,
    source_path: &Path,
) -> anyhow::Result<()> {
    ensure!(
        guest_path.starts_with('/'),
        "guest path must be absolute: `{guest_path}`"
    );

    let commands = vec![
        format!("rm {guest_path}"),
        format!("write {} {guest_path}", source_path.display()),
    ];
    #[cfg(unix)]
    let commands = {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(source_path)
            .with_context(|| format!("failed to stat {}", source_path.display()))?
            .permissions()
            .mode();
        let mut commands = commands;
        commands.push(format!("sif {guest_path} mode 0{mode:o}"));
        commands
    };

    run_debugfs_script(
        rootfs_img,
        &commands,
        &format!(
            "failed to replace {guest_path} in {} with {}",
            rootfs_img.display(),
            source_path.display()
        ),
    )
}

/// Extracts the contents of a rootfs image into a host staging directory.
pub(crate) fn extract_rootfs(rootfs_img: &Path, output_dir: &Path) -> anyhow::Result<()> {
    let extracted = Command::new("debugfs")
        .arg("-R")
        .arg(format!("rdump / {}", output_dir.display()))
        .arg(rootfs_img)
        .status()
        .with_context(|| format!("failed to spawn debugfs for {}", rootfs_img.display()))?
        .success();
    ensure!(
        extracted,
        "failed to extract {} into {}",
        rootfs_img.display(),
        output_dir.display()
    );
    relativize_absolute_symlinks(output_dir)
}

/// Rewrites absolute symlinks in an extracted staging root as equivalent
/// relative links.
///
/// `debugfs rdump` preserves the guest image's absolute symlink targets (for
/// example `/usr/lib/libz.so.1 -> /usr/lib/libz.so.1.3.2`). The staging root is
/// then used as a `qemu-user` sysroot with no chroot, where an absolute target
/// resolves against the host root and dangles, so dynamic loads such as apk's
/// `libz` fail. Relative targets resolve within the staging root and remain
/// valid both here and, after re-injection, inside the guest.
fn relativize_absolute_symlinks(root: &Path) -> anyhow::Result<()> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in
            fs::read_dir(&dir).with_context(|| format!("failed to read {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .with_context(|| format!("failed to inspect {}", path.display()))?;
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if !file_type.is_symlink() {
                continue;
            }
            let target = fs::read_link(&path)
                .with_context(|| format!("failed to read symlink {}", path.display()))?;
            let Ok(guest_target) = target.strip_prefix("/") else {
                continue;
            };
            let in_root = root.join(guest_target);
            let (Some(link_dir), true) = (path.parent(), in_root.exists()) else {
                continue;
            };
            let relative = relative_symlink_target(link_dir, &in_root);
            fs::remove_file(&path)
                .with_context(|| format!("failed to replace symlink {}", path.display()))?;
            std::os::unix::fs::symlink(&relative, &path).with_context(|| {
                format!(
                    "failed to relink {} -> {}",
                    path.display(),
                    relative.display()
                )
            })?;
        }
    }
    Ok(())
}

/// Computes a path to `to` relative to `from_dir`; both are absolute host paths
/// sharing the staging-root prefix.
fn relative_symlink_target(from_dir: &Path, to: &Path) -> PathBuf {
    let from: Vec<_> = from_dir.components().collect();
    let to: Vec<_> = to.components().collect();
    let shared = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut relative = PathBuf::new();
    for _ in shared..from.len() {
        relative.push("..");
    }
    for component in &to[shared..] {
        relative.push(component.as_os_str());
    }
    // A link to its own parent directory yields no components; `.` points there.
    if relative.as_os_str().is_empty() {
        relative.push(".");
    }
    relative
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

    // First pass: directories and regular files (symlinks need their targets to
    // exist first, because debugfs `symlink` validates the target).
    for entry in &entries {
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

        if file_type.is_symlink() {
            // Defer symlinks to second pass
            continue;
        }

        ensure!(
            file_type.is_file(),
            "unsupported overlay entry `{}`; only regular files, directories, and symlinks are \
             supported",
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

    // Second pass: symlinks (now all targets exist).
    // debugfs symlink syntax (v1.47.0): symlink <link_path> <target_content>
    // The 1st argument is where to create the symlink, the 2nd is what it
    // points to (contrary to the man page which swaps them).
    for entry in &entries {
        let file_name = PathBuf::from(entry.file_name());
        let relative_path = relative_dir.join(&file_name);
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", entry.path().display()))?;

        if file_type.is_symlink() {
            let host_target = fs::read_link(entry.path())
                .with_context(|| format!("failed to read symlink {}", entry.path().display()))?;
            // Convert relative symlink target to absolute guest path so the
            // resulting symlink resolves correctly from any CWD.
            let guest_filespec = if host_target.is_relative() {
                let guest_dir = Path::new("/").join(relative_dir);
                guest_dir.join(&host_target)
            } else {
                host_target.clone()
            };
            commands.push(format!("rm /{}", relative_path.display()));
            commands.push(format!(
                "symlink /{} {}",
                relative_path.display(),
                guest_filespec.display()
            ));
        }
    }

    Ok(())
}

/// Executes a generated `debugfs` script against a writable rootfs image.
///
/// Stderr lines that only report that a directory already exists are suppressed
/// because `mkdir /usr/bin` is harmless when the directory is already present.
/// All other stderr output is forwarded so genuine errors remain visible.
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
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn debugfs for {}", rootfs_img.display()))?;

    // Start draining stderr on a background thread BEFORE writing stdin.
    // Without this ordering, a classic pipe deadlock occurs: debugfs fills the
    // stderr pipe while we are still writing stdin, which causes debugfs to
    // block on its stderr write, which causes it to stop reading stdin, which
    // causes our stdin write to block — a deadlock.  Draining stderr
    // concurrently with stdin writes prevents the pipe from filling up.
    let stderr_handle = child
        .stderr
        .take()
        .context("failed to open debugfs stderr")?;
    let filter_handle = thread::spawn(move || {
        let reader = BufReader::new(stderr_handle);
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            if line.contains("File exists") || line.contains("already exists") {
                continue;
            }
            eprintln!("{line}");
        }
    });

    {
        let mut stdin = child.stdin.take().context("failed to open debugfs stdin")?;
        for command in commands {
            writeln!(stdin, "{command}").context("failed to write debugfs command")?;
        }
        writeln!(stdin, "quit").context("failed to finalize debugfs script")?;
    }

    let status = child.wait().context("failed to wait for debugfs")?;
    let _ = filter_handle.join();

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

    /// Symlinks are written after regular files (two-pass) with the correct
    /// debugfs syntax: `symlink <link_path> <target_content>`.
    /// Relative targets are converted to absolute guest paths.
    #[cfg(unix)]
    #[test]
    fn symlinks_are_emitted_after_regular_files() {
        use std::os::unix;

        let root = tempdir().unwrap();
        let overlay_dir = root.path().join("overlay");
        let lib = overlay_dir.join("usr/lib");
        fs::create_dir_all(&lib).unwrap();

        // ldconfig-style chain: libfoo.so -> libfoo.so.1 -> libfoo.so.1.2.0
        fs::write(lib.join("libfoo.so.1.2.0"), b"elf").unwrap();
        unix::fs::symlink("libfoo.so.1.2.0", lib.join("libfoo.so.1")).unwrap();
        unix::fs::symlink("libfoo.so.1", lib.join("libfoo.so")).unwrap();

        let mut commands = Vec::new();
        collect_overlay_debugfs_commands(&overlay_dir, Path::new(""), &mut commands).unwrap();

        let write_pos = commands
            .iter()
            .position(|c| c.contains("libfoo.so.1.2.0") && c.starts_with("write "))
            .unwrap();
        let sym1_pos = commands
            .iter()
            .position(|c| c == "symlink /usr/lib/libfoo.so.1 /usr/lib/libfoo.so.1.2.0")
            .unwrap();
        let sym0_pos = commands
            .iter()
            .position(|c| c == "symlink /usr/lib/libfoo.so /usr/lib/libfoo.so.1")
            .unwrap();

        assert!(
            sym1_pos > write_pos,
            "symlink must be second pass, after its target"
        );
        assert!(
            sym0_pos > write_pos,
            "symlink must be second pass, after its target"
        );
    }
}
