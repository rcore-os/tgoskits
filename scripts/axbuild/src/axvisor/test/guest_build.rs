//! Build source guests through the existing ArceOS build pipeline before the host.
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail, ensure};
use serde::Deserialize;

use crate::context::{AppContext, BuildCliArgs};

const GUEST_BUILDS_FILE: &str = "guest-builds.toml";
/// Environment variable carrying the absolute build target directory to
/// `host_prepare`.
const TARGET_DIR_ENV: &str = "AXBUILD_TARGET_DIR";

/// Optional per-case preparation declared next to a run configuration.
///
/// Both lists are optional: a case may only need source guests, only need
/// shared resources, or neither.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GuestBuilds {
    #[serde(default)]
    arceos_build_configs: Vec<PathBuf>,
    /// Resource preparation command, as an argv without a shell. It runs from
    /// the workspace root with [`TARGET_DIR_ENV`] pointing at the build target
    /// directory, and later steps only read its outputs after it succeeded.
    #[serde(default)]
    host_prepare: Option<Vec<String>>,
}

pub(crate) async fn prepare(app: &mut AppContext, run_config: &Path) -> anyhow::Result<()> {
    let Some(builds) = load_guest_builds(run_config)? else {
        return Ok(());
    };
    run_host_prepare(
        app.workspace_root(),
        app.target_dir(),
        builds.host_prepare.as_deref(),
    )?;
    let directory = run_config
        .parent()
        .context("run configuration has no directory")?;
    for path in builds.arceos_build_configs {
        let config = directory.join(path).canonicalize()?;
        ensure!(
            config.starts_with(app.workspace_root()),
            "guest config escapes workspace"
        );
        let guest = crate::arceos::build::load_arceos_build_file(&config)?;
        ensure!(
            guest.package.is_some() && guest.target.is_some(),
            "test guest config must declare package and target"
        );
        let (request, _) = app.prepare_arceos_request(
            BuildCliArgs {
                config: Some(config),
                ..Default::default()
            },
            None,
            None,
            crate::arceos::build::resolve_build_info_path,
        )?;
        let cargo = crate::arceos::build::load_cargo_config(&request, app.workspace_context())?;
        ensure!(cargo.to_bin, "board guest build must enable to_bin");
        println!("prepare board guest: {}", request.package);
        app.build(cargo, request.build_info_path).await?;
    }
    Ok(())
}

/// Reads `guest-builds.toml` next to a run configuration.
///
/// A case without that file declares no preparation, so the caller continues
/// with the build configuration alone.
fn load_guest_builds(run_config: &Path) -> anyhow::Result<Option<GuestBuilds>> {
    let directory = run_config
        .parent()
        .context("run configuration has no directory")?;
    let path = directory.join(GUEST_BUILDS_FILE);
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    toml::from_str(&contents)
        .with_context(|| format!("invalid guest build list {}", path.display()))
        .map(Some)
}

/// Runs the resource preparation command a case declared, if any.
///
/// `None` means the case declared no preparation. The argv is spawned directly
/// from the workspace root, never through a shell, with
/// [`TARGET_DIR_ENV`] pointing at the absolute build target directory. A
/// non-zero exit status is an error, so a partially written resource directory
/// never reaches the kernel build or the boot payload.
fn run_host_prepare(
    workspace_root: &Path,
    target_dir: &Path,
    host_prepare: Option<&[String]>,
) -> anyhow::Result<()> {
    let Some(argv) = host_prepare else {
        return Ok(());
    };
    let (program, args) = argv
        .split_first()
        .context("host_prepare must contain a program and its arguments")?;
    ensure!(!program.is_empty(), "host_prepare program is empty");
    let target_dir = std::path::absolute(target_dir)
        .with_context(|| format!("failed to resolve target dir {}", target_dir.display()))?;
    println!("prepare case resources: {program}");
    let status = Command::new(program)
        .args(args)
        .current_dir(workspace_root)
        .env(TARGET_DIR_ENV, &target_dir)
        .status()
        .with_context(|| format!("failed to run host_prepare `{program}`"))?;
    if !status.success() {
        bail!("host_prepare `{program}` failed with status {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[cfg(unix)]
    fn write_script(directory: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = directory.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn case_without_a_sidecar_is_not_prepared() {
        let root = tempdir().unwrap();
        let run_config = root.path().join("qemu-aarch64.toml");

        assert!(load_guest_builds(&run_config).unwrap().is_none());
        run_host_prepare(root.path(), root.path(), None).unwrap();
        assert!(
            fs::read_dir(root.path()).unwrap().next().is_none(),
            "a case that declares nothing must not run anything"
        );
    }

    #[cfg(unix)]
    #[test]
    fn declared_preparation_runs_from_the_workspace_root_with_the_absolute_target_dir() {
        let root = tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let target_dir = root.path().join("workspace/target");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&target_dir).unwrap();
        let script = write_script(
            &workspace,
            "prepare.sh",
            concat!(
                "printf '%s' \"$AXBUILD_TARGET_DIR\" > target-dir.txt\n",
                "printf '%s' \"$PWD\" > pwd.txt\n",
                "printf '%s' \"$1\" > arg.txt\n",
            ),
        );
        // Declaring only `host_prepare` keeps the other sidecar field optional.
        fs::write(
            workspace.join(GUEST_BUILDS_FILE),
            format!("host_prepare = [\"{}\", \"extra\"]\n", script.display()),
        )
        .unwrap();
        let run_config = workspace.join("qemu-aarch64.toml");
        let builds = load_guest_builds(&run_config).unwrap().unwrap();

        run_host_prepare(&workspace, &target_dir, builds.host_prepare.as_deref()).unwrap();

        assert_eq!(
            fs::read_to_string(workspace.join("target-dir.txt")).unwrap(),
            target_dir.display().to_string()
        );
        // The shell reports the resolved working directory, which may differ
        // from the temporary directory spelling on hosts with symlinked
        // temporary roots.
        assert_eq!(
            fs::read_to_string(workspace.join("pwd.txt")).unwrap(),
            fs::canonicalize(&workspace).unwrap().display().to_string()
        );
        assert_eq!(
            fs::read_to_string(workspace.join("arg.txt")).unwrap(),
            "extra"
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_preparation_is_reported_instead_of_reading_partial_resources() {
        let root = tempdir().unwrap();
        let workspace = root.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let script = write_script(
            &workspace,
            "prepare.sh",
            "printf 'partial' > resources.bin\nexit 3\n",
        );
        fs::write(
            workspace.join(GUEST_BUILDS_FILE),
            format!("host_prepare = [\"{}\"]\n", script.display()),
        )
        .unwrap();
        let run_config = workspace.join("qemu-aarch64.toml");
        let builds = load_guest_builds(&run_config).unwrap().unwrap();

        let error = run_host_prepare(
            &workspace,
            &workspace.join("target"),
            builds.host_prepare.as_deref(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("prepare.sh"));
        assert!(error.to_string().contains("3"));
    }
}
