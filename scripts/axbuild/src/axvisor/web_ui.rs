//! Frontend orchestration for the Axvisor dashboard (`web-ui` feature).
//!
//! axbuild is the build orchestrator, so — following the Tauri/Trunk pattern —
//! the React bundle is produced here, before cargo runs, and then packed into
//! the host initramfs by [`super::bundle`]. npm always runs against a copy of
//! `os/axvisor/web-ui` under the workspace target directory: `node_modules`
//! and `dist` never touch the source tree, and a clean checkout needs no
//! manual frontend step.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail, ensure};
use walkdir::WalkDir;

/// Cargo feature that turns on the dashboard build and the serving routes.
pub(super) const WEB_UI_FEATURE: &str = "web-ui";

/// Minimum tool majors from `web-ui/package.json` `engines`. The CI bootstrap
/// provisions Node 24.11.1, whose bundled npm is 11.x but older than the
/// patch engines mentions; major checks are what the build actually needs.
const MIN_NODE_MAJOR: u32 = 24;
const MIN_NPM_MAJOR: u32 = 11;

/// Directories that belong to a previous build only and must never be copied
/// from the source tree into the staging area.
const STALE_OUTPUT_DIRS: [&str; 2] = ["node_modules", "dist"];

/// Returns the dashboard feature state of a loaded cargo build config.
pub(super) fn enabled(features: &[String]) -> bool {
    features.iter().any(|feature| feature == WEB_UI_FEATURE)
}

/// Builds the dashboard when the feature is enabled and returns its `dist`
/// tree for the initramfs packer; `None` means this build carries no UI.
pub(super) fn build_dist(
    features: &[String],
    axvisor_dir: &Path,
    target_dir: &Path,
) -> anyhow::Result<Option<PathBuf>> {
    if !enabled(features) {
        return Ok(None);
    }
    let source = axvisor_dir.join("web-ui");
    ensure!(
        source.join("package.json").is_file(),
        "the `web-ui` feature needs the frontend project at {}",
        source.display()
    );
    let staging = target_dir.join("axbuild").join("axvisor").join("web-ui");
    stage_sources(&source, &staging)?;
    check_toolchain()?;
    install_dependencies(&source, &staging)?;
    run_build(&staging)?;
    let dist = staging.join("dist");
    ensure!(
        dist.join("index.html").is_file(),
        "the frontend build produced no dist/index.html under {}",
        staging.display()
    );
    println!("[axbuild] web-ui dist={}", dist.display());
    Ok(Some(dist))
}

/// Copies the frontend project into the staging area, leaving any previous
/// `node_modules` and build output in place.
fn stage_sources(source: &Path, staging: &Path) -> anyhow::Result<()> {
    for entry in WalkDir::new(source)
        .min_depth(1)
        .into_iter()
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            entry.depth() == 0 || !STALE_OUTPUT_DIRS.contains(&name.as_ref())
        })
    {
        let entry = entry?;
        let relative = entry.path().strip_prefix(source)?;
        let destination = staging.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&destination)?;
        } else {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), &destination)?;
        }
    }
    Ok(())
}

/// Fails with an actionable message when node/npm cannot serve the project's
/// `engines` contract.
fn check_toolchain() -> anyhow::Result<()> {
    let node = tool_major("node")?;
    ensure!(
        node.map(|major| major >= MIN_NODE_MAJOR).unwrap_or(false),
        "node >= {MIN_NODE_MAJOR} is required to build the `web-ui` dashboard (see \
         os/axvisor/web-ui/package.json); install it or drop the feature"
    );
    let npm = tool_major("npm")?;
    ensure!(
        npm.map(|major| major >= MIN_NPM_MAJOR).unwrap_or(false),
        "npm >= {MIN_NPM_MAJOR} is required to build the `web-ui` dashboard (see \
         os/axvisor/web-ui/package.json); install it or drop the feature"
    );
    Ok(())
}

fn tool_major(tool: &str) -> anyhow::Result<Option<u32>> {
    Ok(full_tool_version(tool)?.map(|(major, ..)| major))
}

fn full_tool_version(tool: &str) -> anyhow::Result<Option<(u32, u32, u32)>> {
    let output = Command::new(tool)
        .arg("--version")
        .output()
        .with_context(|| format!("run `{tool} --version`; is {tool} installed and on PATH?"))?;
    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let mut parts = version
        .trim_start_matches(['v', 'V'])
        .split('.')
        .map(|part| part.trim().parse::<u32>());
    let Some(Some(major)) = parts.next().map(Result::ok) else {
        bail!("unrecognized `{tool} --version` output: {version:?}")
    };
    let part = |part: Option<Result<u32, _>>| part.and_then(Result::ok).unwrap_or(0);
    let minor = part(parts.next());
    let patch = part(parts.next());
    Ok(Some((major, minor, patch)))
}

/// Runs `npm ci` only when the lockfile changed since the last install.
fn install_dependencies(source: &Path, staging: &Path) -> anyhow::Result<()> {
    let lockfile = source.join("package-lock.json");
    let current = fs::read(&lockfile).with_context(|| format!("read {}", lockfile.display()))?;
    if dependencies_fresh(&current, staging) {
        return Ok(());
    }
    run_npm(staging, &["ci", "--no-fund", "--no-audit"])?;
    fs::write(staging.join(".package-lock.stamp"), current)?;
    Ok(())
}

/// The staged install is fresh when its lockfile stamp matches the source
/// lockfile and `node_modules` still exists.
fn dependencies_fresh(lockfile: &[u8], staging: &Path) -> bool {
    fs::read(staging.join(".package-lock.stamp")).is_ok_and(|stamped| stamped == lockfile)
        && staging.join("node_modules").is_dir()
}

fn run_build(staging: &Path) -> anyhow::Result<()> {
    run_npm(staging, &["run", "build"])
}

fn run_npm(staging: &Path, arguments: &[&'static str]) -> anyhow::Result<()> {
    let mut command = Command::new("npm");
    command
        .args(arguments)
        .current_dir(staging)
        .stdin(std::process::Stdio::inherit());
    let summary = format!("npm {} in {}", arguments.join(" "), staging.display());
    let status = command
        .status()
        .with_context(|| format!("spawn {summary}; is npm installed and on PATH?"))?;
    ensure!(status.success(), "{summary} failed with {status}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_detection_matches_exact_name() {
        assert!(enabled(&["web-ui".to_string()]));
        assert!(enabled(&[
            "ax-driver/nvme".to_string(),
            "web-ui".to_string()
        ]));
        assert!(!enabled(&["web".to_string()]));
        assert!(!enabled(&[]));
    }

    #[test]
    fn staging_skips_previous_build_outputs_and_keeps_them() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let staging = root.path().join("staging");
        fs::create_dir_all(source.join("src/app")).unwrap();
        fs::write(source.join("package.json"), b"{}").unwrap();
        fs::write(source.join("src/app/main.tsx"), b"app").unwrap();
        fs::create_dir_all(source.join("dist")).unwrap();
        fs::write(source.join("dist/index.html"), b"stale").unwrap();
        fs::create_dir_all(source.join("node_modules/.bin")).unwrap();
        fs::write(source.join("node_modules/.bin/vite"), b"binary").unwrap();

        stage_sources(&source, &staging).unwrap();

        assert_eq!(
            fs::read_to_string(staging.join("src/app/main.tsx")).unwrap(),
            "app"
        );
        assert!(!staging.join("dist").exists());
        assert!(!staging.join("node_modules").exists());

        // A re-stage over an area that already ran a build keeps that build's
        // outputs: the copy never deletes what npm owns.
        fs::create_dir_all(staging.join("node_modules/.bin")).unwrap();
        fs::write(staging.join("node_modules/.bin/vite"), b"binary").unwrap();
        fs::create_dir_all(staging.join("dist")).unwrap();
        fs::write(staging.join("dist/index.html"), b"fresh").unwrap();
        stage_sources(&source, &staging).unwrap();
        assert_eq!(
            fs::read_to_string(staging.join("node_modules/.bin/vite")).unwrap(),
            "binary"
        );
        assert_eq!(
            fs::read_to_string(staging.join("dist/index.html")).unwrap(),
            "fresh"
        );
    }

    #[test]
    fn dependency_install_runs_only_when_the_lockfile_changes() {
        let root = tempfile::tempdir().unwrap();
        let staging = root.path().join("staging");
        fs::create_dir_all(&staging).unwrap();

        // No stamp yet: an install is required.
        assert!(!dependencies_fresh(b"lock-v1", &staging));

        // Simulate a completed install: the stamp and node_modules exist, so
        // the decision must skip npm without executing it.
        fs::write(staging.join(".package-lock.stamp"), b"lock-v1").unwrap();
        fs::create_dir_all(staging.join("node_modules")).unwrap();
        assert!(dependencies_fresh(b"lock-v1", &staging));

        // A changed lockfile invalidates the stamp, and so does a wiped
        // node_modules even with a matching stamp.
        assert!(!dependencies_fresh(b"lock-v2", &staging));
        fs::remove_dir_all(staging.join("node_modules")).unwrap();
        assert!(!dependencies_fresh(b"lock-v1", &staging));
    }
}
