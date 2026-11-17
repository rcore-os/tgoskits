use std::{path::Path, process::Command};

use anyhow::{Context, Result, ensure};

use super::process::ProcessExt;

/// Ensures the active toolchain provides a target without installing into a CI image.
pub(crate) fn ensure_target(target: &str) -> Result<()> {
    let prepared_image = std::env::var("TGOSKITS_PREBUILT_CONTAINER").as_deref() == Ok("1");
    ensure_target_with_rustup(target, prepared_image, Path::new("rustup"))
}

fn ensure_target_with_rustup(
    target: &str,
    prepared_image: bool,
    rustup_executable: &Path,
) -> Result<()> {
    if prepared_image {
        let output = Command::new(rustup_executable)
            .args(["target", "list", "--installed"])
            .output()
            .context("failed to query installed Rust targets")?;
        ensure!(
            output.status.success(),
            "failed to query installed Rust targets ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
        ensure!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|installed| installed.trim() == target),
            "prepared container is missing Rust target `{target}`; rebuild the CI image"
        );
        return Ok(());
    }

    Command::new(rustup_executable)
        .args(["target", "add", target])
        .exec()
        .with_context(|| format!("failed to install Rust target `{target}` via rustup"))
}

#[cfg(all(test, unix))]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

    use tempfile::TempDir;

    use super::ensure_target_with_rustup;

    fn rustup_fixture(output: &str, status: i32) -> (TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("rustup");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$0.calls\"\nprintf '%s' '{}'\nexit {status}\n",
            output.replace('\'', "'\\''")
        );
        fs::write(&executable, script).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        (directory, executable)
    }

    #[test]
    fn prepared_images_only_accept_an_installed_target_from_a_successful_query() {
        let target = "fixture-unknown-linux-musl";
        for (output, status, available) in [
            ("another-target\nfixture-unknown-linux-musl\n", 0, true),
            ("fixture-unknown-linux-musl-extra\n", 0, false),
            ("", 0, false),
            ("fixture-unknown-linux-musl\n", 7, false),
        ] {
            let (_directory, executable) = rustup_fixture(output, status);
            let _script_writer = fs::OpenOptions::new()
                .write(true)
                .open(&executable)
                .unwrap();
            let result = ensure_target_with_rustup(target, true, &executable);
            assert_eq!(
                result.is_ok(),
                available,
                "output={output:?}, status={status}, result={result:?}"
            );
            let calls = fs::read_to_string(executable.with_extension("calls")).unwrap();
            assert_eq!(calls, "target list --installed\n");
        }
    }

    #[test]
    fn bare_hosts_keep_target_installation_and_propagate_failure() {
        for status in [0, 7] {
            let (_directory, executable) = rustup_fixture("", status);
            let result = ensure_target_with_rustup("fixture-target", false, &executable);
            assert_eq!(result.is_ok(), status == 0);
            let calls = fs::read_to_string(executable.with_extension("calls")).unwrap();
            assert_eq!(calls, "target add fixture-target\n");
        }
    }
}
