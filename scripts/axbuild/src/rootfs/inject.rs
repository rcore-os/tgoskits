//! Rootfs image content extraction and overlay injection helpers.
//!
//! Main responsibilities:
//! - Use `debugfs` (under `fakeroot` when host ownership cannot be restored
//!   directly) to extract a rootfs image into a staging directory
//! - Write overlay files and directories back into a rootfs image
//! - Generate and execute `debugfs` scripts for image content updates
//!
//! Unlike [`super::qemu`], this file operates on the contents of the rootfs
//! image itself.

use std::{
    fs,
    io::{self, BufRead, BufReader, Write},
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
    let Some(contents) = read_binary_file(rootfs_img, guest_path)? else {
        return Ok(None);
    };

    String::from_utf8(contents)
        .map(Some)
        .with_context(|| format!("{}:{guest_path} is not valid UTF-8", rootfs_img.display()))
}

/// Reads a binary file from a rootfs image with `debugfs`.
///
/// Returns `Ok(None)` when the image is readable but the guest path does not
/// exist.
pub(crate) fn read_binary_file(
    rootfs_img: &Path,
    guest_path: &str,
) -> anyhow::Result<Option<Vec<u8>>> {
    ensure!(
        guest_path.starts_with('/'),
        "guest path must be absolute: `{guest_path}`"
    );

    let output = Command::new("debugfs")
        .arg("-R")
        .arg(format!("cat {}", debugfs_argument(guest_path)?))
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

    Ok(Some(output.stdout))
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
        format!("rm {}", debugfs_argument(guest_path)?),
        format!(
            "write {} {}",
            debugfs_path_argument(source_path)?,
            debugfs_argument(guest_path)?
        ),
    ];
    #[cfg(unix)]
    let commands = {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(source_path)
            .with_context(|| format!("failed to stat {}", source_path.display()))?
            .permissions()
            .mode();
        let mut commands = commands;
        commands.push(format!(
            "sif {} mode 0{mode:o}",
            debugfs_argument(guest_path)?
        ));
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
    #[cfg(unix)]
    let fakeroot_program = current_process_requires_fakeroot().then_some(Path::new("fakeroot"));
    #[cfg(not(unix))]
    let fakeroot_program = None;

    RootfsExtraction {
        rootfs_img,
        output_dir,
        debugfs_program: Path::new("debugfs"),
        fakeroot_program,
    }
    .run()?;
    relativize_absolute_symlinks(output_dir)
}

/// A preselected rootfs extraction command.
///
/// `debugfs rdump` always attempts to restore inode ownership. Callers that
/// cannot safely perform those `chown` calls therefore run it inside
/// `fakeroot` before `debugfs` starts. There is intentionally no
/// direct-execution fallback: a missing `fakeroot` fails before extraction
/// instead of producing thousands of permission warnings and continuing with
/// partially restored metadata.
struct RootfsExtraction<'a> {
    rootfs_img: &'a Path,
    output_dir: &'a Path,
    debugfs_program: &'a Path,
    fakeroot_program: Option<&'a Path>,
}

impl RootfsExtraction<'_> {
    fn run(&self) -> anyhow::Result<()> {
        let mut command = self.command();
        let rendered_command = format!("{command:?}");
        let output = command.output().with_context(|| {
            if let Some(fakeroot) = self.fakeroot_program {
                format!(
                    "failed to spawn fakeroot `{}`; rootfs extraction without full host ownership \
                     privileges requires fakeroot",
                    fakeroot.display()
                )
            } else {
                format!("failed to spawn debugfs for {}", self.rootfs_img.display())
            }
        })?;

        if output.status.success() {
            return Ok(());
        }

        eprintln!("rootfs extraction command failed: {rendered_command}");
        io::stdout()
            .write_all(&output.stdout)
            .context("failed to replay rootfs extraction stdout")?;
        io::stderr()
            .write_all(&output.stderr)
            .context("failed to replay rootfs extraction stderr")?;
        bail!(
            "failed to extract {} into {}: command exited with status {}",
            self.rootfs_img.display(),
            self.output_dir.display(),
            output.status
        );
    }

    fn command(&self) -> Command {
        let mut command = if let Some(fakeroot) = self.fakeroot_program {
            let mut command = Command::new(fakeroot);
            command.arg("--").arg(self.debugfs_program);
            command
        } else {
            Command::new(self.debugfs_program)
        };
        command
            .arg("-R")
            .arg(format!("rdump / {}", self.output_dir.display()))
            .arg(self.rootfs_img);
        command
    }
}

#[cfg(unix)]
fn effective_uid() -> libc::uid_t {
    // SAFETY: `geteuid` has no arguments or caller-side safety preconditions.
    unsafe { libc::geteuid() }
}

#[cfg(unix)]
fn current_process_requires_fakeroot() -> bool {
    #[cfg(target_os = "linux")]
    {
        requires_fakeroot(
            effective_uid(),
            linux_id_map_is_full_identity("/proc/self/uid_map"),
            linux_id_map_is_full_identity("/proc/self/gid_map"),
            linux_has_effective_cap_chown(),
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        effective_uid() != 0
    }
}

#[cfg(target_os = "linux")]
fn requires_fakeroot(
    effective_uid: libc::uid_t,
    full_uid_map: bool,
    full_gid_map: bool,
    has_effective_cap_chown: bool,
) -> bool {
    effective_uid != 0 || !full_uid_map || !full_gid_map || !has_effective_cap_chown
}

#[cfg(target_os = "linux")]
fn linux_id_map_is_full_identity(path: &str) -> bool {
    fs::read_to_string(path)
        .ok()
        .is_some_and(|contents| id_map_is_full_identity(&contents))
}

#[cfg(target_os = "linux")]
fn id_map_is_full_identity(contents: &str) -> bool {
    let mut lines = contents.lines().filter(|line| !line.trim().is_empty());
    let Some(line) = lines.next() else {
        return false;
    };
    if lines.next().is_some() {
        return false;
    }
    let fields = line
        .split_ascii_whitespace()
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>();
    matches!(fields.as_deref(), Ok([0, 0, 4_294_967_295]))
}

#[cfg(target_os = "linux")]
fn linux_has_effective_cap_chown() -> bool {
    fs::read_to_string("/proc/self/status")
        .ok()
        .is_some_and(|status| status_has_effective_cap_chown(&status))
}

#[cfg(target_os = "linux")]
fn status_has_effective_cap_chown(status: &str) -> bool {
    status
        .lines()
        .find_map(|line| line.strip_prefix("CapEff:"))
        .and_then(|value| u64::from_str_radix(value.trim(), 16).ok())
        .is_some_and(|capabilities| capabilities & 1 != 0)
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
    )?;
    // debugfs can exit successfully after an individual command failed. Make
    // the image durable and prove every regular overlay file before QEMU or a
    // post-injection cache consumes it.
    fs::File::open(rootfs_img)
        .with_context(|| format!("failed to open {} for sync", rootfs_img.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync injected image {}", rootfs_img.display()))?;
    verify_overlay_regular_files(rootfs_img, overlay_dir, Path::new(""))
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
            commands.push(format!(
                "mkdir {}",
                debugfs_guest_path_argument(&relative_path)?
            ));
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
        let guest_path = debugfs_guest_path_argument(&relative_path)?;
        commands.push(format!("rm {guest_path}"));
        commands.push(format!(
            "write {} {guest_path}",
            debugfs_path_argument(&entry.path())?
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = fs::metadata(entry.path())
                .with_context(|| format!("failed to stat {}", entry.path().display()))?;
            commands.push(format!(
                "sif {guest_path} mode 0{:o}",
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
            let guest_path = debugfs_guest_path_argument(&relative_path)?;
            commands.push(format!("rm {guest_path}"));
            commands.push(format!(
                "symlink {guest_path} {}",
                debugfs_path_argument(&guest_filespec)?
            ));
        }
    }

    Ok(())
}

fn verify_overlay_regular_files(
    rootfs_img: &Path,
    overlay_dir: &Path,
    relative_dir: &Path,
) -> anyhow::Result<()> {
    let current_dir = overlay_dir.join(relative_dir);
    let mut entries = fs::read_dir(&current_dir)
        .with_context(|| format!("failed to read {}", current_dir.display()))?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("failed to read {}", current_dir.display()))?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let relative_path = relative_dir.join(entry.file_name());
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", entry.path().display()))?;
        if file_type.is_dir() {
            verify_overlay_regular_files(rootfs_img, overlay_dir, &relative_path)?;
            continue;
        }
        if file_type.is_symlink() {
            continue;
        }

        let host_contents = fs::read(entry.path())
            .with_context(|| format!("failed to read {}", entry.path().display()))?;
        let guest_path = overlay_guest_path(&relative_path)?;
        let image_contents = read_binary_file(rootfs_img, &guest_path)?.with_context(|| {
            format!(
                "overlay injection did not create {guest_path} in {}",
                rootfs_img.display()
            )
        })?;
        ensure!(
            image_contents == host_contents,
            "overlay injection content mismatch for {guest_path} in {}",
            rootfs_img.display()
        );
    }

    Ok(())
}

fn debugfs_guest_path_argument(relative_path: &Path) -> anyhow::Result<String> {
    debugfs_argument(&overlay_guest_path(relative_path)?)
}

fn overlay_guest_path(relative_path: &Path) -> anyhow::Result<String> {
    let relative_path = relative_path.to_str().with_context(|| {
        format!(
            "overlay guest path is not valid UTF-8: {}",
            relative_path.display()
        )
    })?;
    Ok(format!("/{relative_path}"))
}

fn debugfs_path_argument(path: &Path) -> anyhow::Result<String> {
    let path = path
        .to_str()
        .with_context(|| format!("debugfs path is not valid UTF-8: {}", path.display()))?;
    debugfs_argument(path)
}

fn debugfs_argument(argument: &str) -> anyhow::Result<String> {
    ensure!(
        !argument.contains(['\0', '\n', '\r']),
        "debugfs argument contains an unsupported control character"
    );
    Ok(format!(
        "\"{}\"",
        argument.replace('\\', "\\\\").replace('"', "\\\"")
    ))
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
    run_debugfs_script_with_program(Path::new("debugfs"), rootfs_img, commands, context_message)
}

fn run_debugfs_script_with_program(
    debugfs_program: &Path,
    rootfs_img: &Path,
    commands: &[String],
    context_message: &str,
) -> anyhow::Result<()> {
    eprintln!("debugfs -w {}", rootfs_img.display());
    let mut child = Command::new(debugfs_program)
        .arg("-w")
        .arg(rootfs_img)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
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
    use std::{env, fs, path::Path};

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

        assert_eq!(commands[0], "mkdir \"/usr\"");
        assert!(commands.contains(&"mkdir \"/usr/bin\"".to_string()));
        assert!(commands.contains(&format!(
            "write \"{}\" \"/usr/bin/test-bin\"",
            binary.display()
        )));
        assert!(commands.contains(&"sif \"/usr/bin/test-bin\" mode 0100755".to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn overlay_injection_handles_host_paths_with_spaces() {
        let root = tempdir().unwrap();
        let rootfs_img = root.path().join("rootfs.img");
        let truncate_status = Command::new("truncate")
            .args(["-s", "16M"])
            .arg(&rootfs_img)
            .status()
            .unwrap();
        assert!(truncate_status.success());
        let mkfs_status = Command::new("mkfs.ext4")
            .args(["-q", "-F"])
            .arg(&rootfs_img)
            .status()
            .unwrap();
        assert!(mkfs_status.success());

        let overlay_dir = root.path().join("overlay source");
        fs::create_dir(&overlay_dir).unwrap();
        fs::write(overlay_dir.join("payload file.bin"), b"injected payload").unwrap();

        inject_overlay(&rootfs_img, &overlay_dir).unwrap();
        assert_eq!(
            read_binary_file(&rootfs_img, "/payload file.bin").unwrap(),
            Some(b"injected payload".to_vec())
        );
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
            .position(|c| c == "symlink \"/usr/lib/libfoo.so.1\" \"/usr/lib/libfoo.so.1.2.0\"")
            .unwrap();
        let sym0_pos = commands
            .iter()
            .position(|c| c == "symlink \"/usr/lib/libfoo.so\" \"/usr/lib/libfoo.so.1\"")
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

    #[cfg(unix)]
    #[test]
    fn non_root_extraction_starts_debugfs_inside_fakeroot() {
        let root = executable_helper_tempdir();
        let fakeroot = root.path().join("fakeroot");
        let debugfs = root.path().join("debugfs");
        let marker = root.path().join("debugfs-ran-inside-fakeroot");
        write_executable(
            &fakeroot,
            "#!/bin/sh\ntest \"$1\" = \"--\" || exit 91\nshift\nexport \
             AXBUILD_TEST_FAKEROOT=1\nexec \"$@\"\n",
        );
        write_executable(
            &debugfs,
            &format!(
                "#!/bin/sh\ntest \"${{AXBUILD_TEST_FAKEROOT:-}}\" = \"1\" || exit 92\ntouch '{}'\n",
                marker.display()
            ),
        );

        let output_dir = root.path().join("staging");
        fs::create_dir(&output_dir).unwrap();
        RootfsExtraction {
            rootfs_img: Path::new("rootfs.img"),
            output_dir: &output_dir,
            debugfs_program: &debugfs,
            fakeroot_program: Some(&fakeroot),
        }
        .run()
        .unwrap();

        assert!(marker.exists());
    }

    #[cfg(unix)]
    #[test]
    fn missing_fakeroot_fails_before_debugfs_starts() {
        let root = tempdir().unwrap();
        let debugfs = root.path().join("debugfs");
        let marker = root.path().join("debugfs-started");
        write_executable(
            &debugfs,
            &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
        );

        let output_dir = root.path().join("staging");
        fs::create_dir(&output_dir).unwrap();
        let error = RootfsExtraction {
            rootfs_img: Path::new("rootfs.img"),
            output_dir: &output_dir,
            debugfs_program: &debugfs,
            fakeroot_program: Some(&root.path().join("missing-fakeroot")),
        }
        .run()
        .unwrap_err();

        assert!(
            error.to_string().contains("failed to spawn fakeroot"),
            "unexpected error: {error:#}"
        );
        assert!(!marker.exists(), "debugfs must not run without fakeroot");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn direct_extraction_requires_full_host_ownership_privileges() {
        assert!(!requires_fakeroot(0, true, true, true));
        assert!(requires_fakeroot(1, true, true, true));
        assert!(requires_fakeroot(0, false, true, true));
        assert!(requires_fakeroot(0, true, false, true));
        assert!(requires_fakeroot(0, true, true, false));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn full_identity_map_rejects_partial_or_split_user_namespaces() {
        assert!(id_map_is_full_identity("0 0 4294967295\n"));
        assert!(!id_map_is_full_identity("0 1000 1\n"));
        assert!(!id_map_is_full_identity("0 1000 1\n1 100000 65535\n"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cap_chown_parser_requires_effective_capability_bit() {
        assert!(status_has_effective_cap_chown(
            "Name:\ttg-xtask\nCapEff:\t0000000000000001\n"
        ));
        assert!(!status_has_effective_cap_chown(
            "Name:\ttg-xtask\nCapEff:\t0000000000000000\n"
        ));
        assert!(!status_has_effective_cap_chown(
            "Name:\ttg-xtask\nCapEff:\tinvalid\n"
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn debugfs_script_discards_normal_stdout_and_receives_all_commands() {
        let root = executable_helper_tempdir();
        let debugfs = root.path().join("debugfs");
        let received_commands = root.path().join("received-commands");
        write_executable(
            &debugfs,
            &format!(
                "#!/bin/sh\ntest \"$(readlink /proc/$$/fd/1)\" = /dev/null || exit 91\ncat > '{}'
",
                received_commands.display()
            ),
        );

        run_debugfs_script_with_program(
            &debugfs,
            Path::new("rootfs.img"),
            &["rm /usr/bin/app".into(), "write app /usr/bin/app".into()],
            "failed to inject test overlay",
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(received_commands).unwrap(),
            "rm /usr/bin/app\nwrite app /usr/bin/app\nquit\n"
        );
    }

    #[cfg(unix)]
    fn executable_helper_tempdir() -> tempfile::TempDir {
        let test_binary = env::current_exe().expect("test binary path must be available");
        let test_binary_dir = test_binary
            .parent()
            .expect("test binary path must have a parent directory");

        tempfile::Builder::new()
            .prefix("axbuild-rootfs-test-")
            .tempdir_in(test_binary_dir)
            .expect("test binary directory must accept temporary helper scripts")
    }

    #[cfg(unix)]
    fn write_executable(path: &Path, contents: &str) {
        use std::os::unix::fs::PermissionsExt;

        fs::write(path, contents).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}
