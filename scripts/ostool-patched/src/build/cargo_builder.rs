//! Cargo build command builder and executor.
//!
//! This module provides the [`CargoBuilder`] type for constructing and executing
//! Cargo build commands with customizable options, environment variables, and
//! pre/post build hooks.

use std::{
    collections::HashMap,
    io::BufReader,
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::{Context, anyhow, bail};
use cargo_metadata::{Message, PackageId};
use colored::Colorize;

use crate::{
    Tool,
    build::{
        config::{Cargo, CargoBuildProfile},
        someboot,
    },
    utils::{Command, PathResultExt},
};

#[derive(Debug, Clone)]
struct ResolvedCargoArtifact {
    elf_path: PathBuf,
    cargo_artifact_dir: PathBuf,
}

/// A builder for constructing and executing Cargo commands.
///
/// `CargoBuilder` provides a fluent API for configuring Cargo build or run
/// commands with custom arguments, environment variables, and build hooks.
///
/// This builder is an internal implementation detail used by [`Tool`].
pub struct CargoBuilder<'a> {
    tool: &'a mut Tool,
    config: &'a Cargo,
    command: String,
    extra_args: Vec<String>,
    extra_envs: HashMap<String, String>,
    skip_objcopy: bool,
    resolve_artifact_from_json: bool,
    resolved_artifact: Option<ResolvedCargoArtifact>,
    config_path: Option<PathBuf>,
}

impl<'a> CargoBuilder<'a> {
    /// Creates a new `CargoBuilder` for executing `cargo build`.
    ///
    /// # Arguments
    ///
    /// * `tool` - The tool context.
    /// * `config` - The Cargo build configuration.
    /// * `config_path` - Optional path to the configuration file.
    pub fn build(tool: &'a mut Tool, config: &'a Cargo, config_path: Option<PathBuf>) -> Self {
        Self {
            tool,
            config,
            command: "build".to_string(),
            extra_args: Vec::new(),
            extra_envs: HashMap::new(),
            skip_objcopy: false,
            resolve_artifact_from_json: true,
            resolved_artifact: None,
            config_path,
        }
    }

    /// Sets the debug mode for the build.
    ///
    /// When enabled, builds in debug mode and enables GDB server for QEMU.
    pub fn debug(self, debug: bool) -> Self {
        self.tool.config.debug = debug;
        self
    }

    /// Creates a build command using the context's stored config path.
    pub fn build_auto(tool: &'a mut Tool, config: &'a Cargo) -> Self {
        let config_path = tool.ctx.build_config_path.clone();
        Self::build(tool, config, config_path)
    }

    /// Sets whether to skip the objcopy step after building.
    pub fn skip_objcopy(mut self, skip: bool) -> Self {
        self.skip_objcopy = skip;
        self
    }

    /// Enables artifact path resolution from Cargo JSON messages.
    pub fn resolve_artifact_from_json(mut self, enable: bool) -> Self {
        self.resolve_artifact_from_json = enable;
        self
    }

    /// Executes the configured Cargo command.
    ///
    /// This runs pre-build commands, executes Cargo, handles output artifacts,
    /// and runs post-build commands.
    ///
    /// # Errors
    ///
    /// Returns an error if any step of the build process fails.
    pub async fn execute(mut self) -> anyhow::Result<()> {
        // 1. Pre-build commands
        self.run_pre_build_cmds()?;

        // 2. Build and run cargo
        self.run_cargo().await?;

        // 3. Handle output
        self.handle_output().await?;

        // 4. Post-build commands
        self.run_post_build_cmds()?;

        Ok(())
    }

    fn run_pre_build_cmds(&mut self) -> anyhow::Result<()> {
        for cmd in &self.config.pre_build_cmds {
            self.tool.shell_run_cmd(cmd)?;
        }
        Ok(())
    }

    async fn run_cargo(&mut self) -> anyhow::Result<()> {
        self.run_cargo_and_resolve_artifact().await
    }

    async fn run_cargo_and_resolve_artifact(&mut self) -> anyhow::Result<()> {
        let (target_pkg_id, default_run) = self.target_package_info()?;
        let mut cmd = self.build_cargo_command().await?;

        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::inherit());
        cmd.print_cmd();

        let mut child = cmd
            .spawn()
            .context("failed to spawn cargo build command for artifact resolution")?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("failed to capture cargo stdout for message parsing"))?;
        let reader = BufReader::new(stdout);

        let mut executable_artifacts: Vec<(String, ResolvedCargoArtifact)> = Vec::new();
        for message in Message::parse_stream(reader) {
            let message = message.context("failed to parse cargo JSON message stream")?;
            match message {
                Message::CompilerArtifact(artifact) => {
                    if artifact.package_id == target_pkg_id
                        && artifact.target.is_bin()
                        && let Some(executable) = artifact.executable
                    {
                        let elf_path = executable.into_std_path_buf();
                        let cargo_artifact_dir = elf_path
                            .parent()
                            .ok_or_else(|| {
                                anyhow!(
                                    "cargo reported executable without parent directory: {}",
                                    elf_path.display()
                                )
                            })?
                            .to_path_buf();
                        executable_artifacts.push((
                            artifact.target.name,
                            ResolvedCargoArtifact {
                                elf_path,
                                cargo_artifact_dir,
                            },
                        ));
                    }
                }
                Message::CompilerMessage(msg) => {
                    if let Some(rendered) = msg.message.rendered {
                        eprint!("{rendered}");
                    }
                }
                Message::TextLine(line) => {
                    println!("{line}");
                }
                _ => {}
            }
        }

        let status = child
            .wait()
            .context("failed waiting for cargo build process")?;
        if !status.success() {
            bail!("failed with status: {status}");
        }

        let resolved = select_executable_artifact(
            &executable_artifacts,
            self.config.bin.as_deref(),
            default_run.as_deref(),
            &self.config.package,
        )?;

        self.resolved_artifact = Some(resolved);
        Ok(())
    }

    async fn build_cargo_command(&mut self) -> anyhow::Result<Command> {
        let mut cmd = self.tool.command("cargo");

        cmd.arg(&self.command);

        for (k, v) in &self.config.env {
            println!("{}", format!("{k}={v}").cyan());
            cmd.env(k, v);
        }
        for (k, v) in &self.extra_envs {
            println!("{}", format!("{k}={v}").cyan());
            cmd.env(k, v);
        }

        // Extra config
        if let Some(extra_config_path) = self.cargo_extra_config().await? {
            cmd.arg("--config");
            cmd.arg(extra_config_path.display().to_string());
        }

        // Package and target
        cmd.arg("-p");
        cmd.arg(&self.config.package);
        if let Some(bin) = &self.config.bin {
            cmd.arg("--bin");
            cmd.arg(bin);
        }
        cmd.arg("--target");
        cmd.arg(&self.config.target);
        cmd.arg("-Z");
        cmd.arg("unstable-options");

        cmd.arg("--target-dir");
        cmd.arg(self.tool.build_dir().display().to_string());

        // Features
        let features = self.build_features();
        if !features.is_empty() {
            cmd.arg("--features");
            cmd.arg(features.join(","));
        }

        // Config args
        for arg in &self.config.args {
            cmd.arg(arg);
        }

        // Auto-detected args from someboot/build-info.toml
        let workspace_manifest = self.tool.workspace_dir().join("Cargo.toml");
        if self.tool.someboot_build_config_enabled(self.config) && workspace_manifest.exists() {
            let detected_args = someboot::detect_build_config_for_package(
                &workspace_manifest,
                &self.config.package,
                &features,
                &self.config.target,
            )
            .with_context(|| {
                format!(
                    "failed to detect someboot build config from {}",
                    workspace_manifest.display()
                )
            })?;
            for arg in detected_args {
                cmd.arg(arg);
            }
        }

        // Release mode
        if self.effective_profile() == CargoBuildProfile::Release {
            cmd.arg("--release");
        }

        cmd.arg("--message-format");
        cmd.arg("json-render-diagnostics");

        // Extra args
        for arg in &self.extra_args {
            cmd.arg(arg);
        }

        Ok(cmd)
    }

    /// Applies the resolved Cargo artifact to the legacy tool runtime state.
    async fn handle_output(&mut self) -> anyhow::Result<()> {
        let resolved = self.resolved_artifact.clone().ok_or_else(|| {
            anyhow!(
                "cargo build finished without a resolved executable artifact for package '{}' and target '{}'",
                self.config.package,
                self.config.target
            )
        })?;

        self.tool.set_elf_artifact_path(resolved.elf_path).await?;
        self.tool.ctx.artifacts.cargo_artifact_dir = Some(resolved.cargo_artifact_dir.clone());
        self.tool.ctx.artifacts.runtime_artifact_dir = Some(resolved.cargo_artifact_dir);

        if self.config.to_bin && !self.skip_objcopy {
            self.tool.objcopy_output_bin()?;
        }

        Ok(())
    }

    fn run_post_build_cmds(&mut self) -> anyhow::Result<()> {
        for cmd in &self.config.post_build_cmds {
            self.tool.shell_run_cmd(cmd)?;
        }
        Ok(())
    }

    fn target_package_info(&self) -> anyhow::Result<(PackageId, Option<String>)> {
        let metadata = self.tool.metadata()?;
        let Some(package) = metadata
            .packages
            .iter()
            .find(|pkg| pkg.name == self.config.package)
        else {
            bail!(
                "package '{}' not found in cargo metadata under {}",
                self.config.package,
                self.tool.manifest_dir().display()
            );
        };
        Ok((package.id.clone(), package.default_run.clone()))
    }

    fn build_features(&self) -> Vec<String> {
        let mut features = self.config.features.clone();
        if let Some(log_level) = self.log_level_feature() {
            features.push(log_level);
        }
        features
    }

    fn effective_profile(&self) -> CargoBuildProfile {
        self.config.profile.unwrap_or_else(|| {
            if self.tool.debug_enabled() {
                CargoBuildProfile::Debug
            } else {
                CargoBuildProfile::Release
            }
        })
    }

    fn log_level_feature(&self) -> Option<String> {
        let level = self.config.log.clone()?;

        let meta = self.tool.metadata().ok()?;
        let pkg = meta
            .packages
            .iter()
            .find(|p| p.name == self.config.package)?;

        let has_log = pkg.dependencies.iter().any(|dep| dep.name == "log");

        if has_log {
            Some(format!(
                "log/{}max_level_{}",
                if self.effective_profile() == CargoBuildProfile::Debug {
                    ""
                } else {
                    "release_"
                },
                format!("{:?}", level).to_lowercase()
            ))
        } else {
            None
        }
    }

    /// Resolves an optional extra Cargo config from a local path or URL.
    async fn cargo_extra_config(&self) -> anyhow::Result<Option<PathBuf>> {
        let s = match self.config.extra_config.as_ref() {
            Some(s) => s,
            None => return Ok(None),
        };

        // Check if it's a URL (starts with http:// or https://)
        if s.starts_with("http://") || s.starts_with("https://") {
            // Convert GitHub URL to raw content URL if needed
            let download_url = Self::convert_to_raw_url(s);

            // Download to temp directory
            match self.download_config_to_temp(&download_url).await {
                Ok(path) => Ok(Some(path)),
                Err(e) => {
                    eprintln!("Failed to download config from {}: {}", s, e);
                    Err(e)
                }
            }
        } else {
            // It's a local path
            let extra = Path::new(s);

            if extra.is_relative() {
                if let Some(ref config_path) = self.config_path {
                    let combined = config_path
                        .parent()
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "invalid config path without parent: {}",
                                config_path.display()
                            )
                        })?
                        .join(extra);
                    Ok(Some(combined))
                } else {
                    Ok(Some(extra.to_path_buf()))
                }
            } else {
                Ok(Some(extra.to_path_buf()))
            }
        }
    }

    /// Convert GitHub URL to raw content URL
    /// Supports:
    /// - https://github.com/user/repo/blob/branch/path/file -> https://raw.githubusercontent.com/user/repo/branch/path/file
    /// - https://raw.githubusercontent.com/... (already raw, no change)
    /// - Other URLs: no change
    fn convert_to_raw_url(url: &str) -> String {
        // Already a raw URL
        if url.contains("raw.githubusercontent.com") || url.contains("raw.github.com") {
            return url.to_string();
        }

        // Convert github.com/user/repo/blob/... to raw.githubusercontent.com/user/repo/...
        if url.contains("github.com") && url.contains("/blob/") {
            let converted = url
                .replace("github.com", "raw.githubusercontent.com")
                .replace("/blob/", "/");
            println!("Converting GitHub URL to raw: {} -> {}", url, converted);
            return converted;
        }

        // Not a GitHub URL or already in correct format
        url.to_string()
    }

    async fn download_config_to_temp(&self, url: &str) -> anyhow::Result<PathBuf> {
        use std::time::SystemTime;

        println!("Downloading cargo config from: {}", url);

        // Get system temp directory
        let temp_dir = std::env::temp_dir();

        // Generate filename with timestamp
        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Extract filename from URL or use default
        let url_path = url.split('/').next_back().unwrap_or("config.toml");
        let filename = format!("cargo_config_{}_{}", timestamp, url_path);
        let target_path = temp_dir.join(filename);

        // Create reqwest client
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to create HTTP client: {}", e))?;

        // Build request with User-Agent for GitHub
        let mut request = client.get(url);

        if url.contains("github.com") || url.contains("githubusercontent.com") {
            // GitHub requires User-Agent
            request = request.header("User-Agent", "ostool-cargo-downloader");
        }

        // Download the file
        let response = request
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to download from {}: {}", url, e))?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!("HTTP error {}: {}", response.status(), url));
        }

        let content = response
            .bytes()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read response body: {}", e))?;

        // Write to temp file
        tokio::fs::write(&target_path, content)
            .await
            .with_path("failed to write downloaded cargo config", &target_path)
            .with_context(|| format!("while downloading cargo config from {url}"))?;

        println!("Config downloaded to: {}", target_path.display());

        Ok(target_path)
    }
}

fn select_executable_artifact(
    executable_artifacts: &[(String, ResolvedCargoArtifact)],
    explicit_bin: Option<&str>,
    default_run: Option<&str>,
    package: &str,
) -> anyhow::Result<ResolvedCargoArtifact> {
    if let Some(bin) = explicit_bin {
        return executable_artifacts
            .iter()
            .rev()
            .find(|(name, _)| name == bin)
            .map(|(_, artifact)| artifact.clone())
            .ok_or_else(|| {
                anyhow!(
                    "binary target `{bin}` was not built for package `{package}`; check system.Cargo.bin or --bin"
                )
            });
    }

    if executable_artifacts.is_empty() {
        bail!(
            "no executable bin artifact found in cargo JSON output for package `{package}`; ostool currently resolves only Cargo bin targets"
        );
    }

    if let Some((_, artifact)) = executable_artifacts
        .iter()
        .rev()
        .find(|(name, _)| name == package)
    {
        return Ok(artifact.clone());
    }

    if let Some(default_bin) = default_run
        && let Some((_, artifact)) = executable_artifacts
            .iter()
            .rev()
            .find(|(name, _)| name == default_bin)
    {
        return Ok(artifact.clone());
    }

    if executable_artifacts.len() == 1 {
        return Ok(executable_artifacts[0].1.clone());
    }

    let bins = executable_artifacts
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    bail!(
        "package `{package}` has multiple binary targets ({bins}); pass system.Cargo.bin or --bin"
    )
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        fs,
        path::{Path, PathBuf},
    };

    use super::{CargoBuilder, ResolvedCargoArtifact, select_executable_artifact};
    use crate::{
        Tool, ToolConfig,
        build::config::{Cargo, CargoBuildProfile},
    };

    fn artifact(name: &str) -> ResolvedCargoArtifact {
        let cargo_artifact_dir = PathBuf::from("/tmp/ostool-target/debug");
        ResolvedCargoArtifact {
            elf_path: cargo_artifact_dir.join(name),
            cargo_artifact_dir,
        }
    }

    fn select(
        artifacts: &[(String, ResolvedCargoArtifact)],
        explicit_bin: Option<&str>,
        default_run: Option<&str>,
        package: &str,
    ) -> anyhow::Result<ResolvedCargoArtifact> {
        select_executable_artifact(artifacts, explicit_bin, default_run, package)
    }

    fn write_someboot_workspace(root: &std::path::Path) {
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\", \"someboot\"]\nresolver = \"3\"\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("app/src")).unwrap();
        fs::write(
            root.join("app/Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nsomeboot = { path = \"../someboot\" }\n",
        )
        .unwrap();
        fs::write(root.join("app/src/main.rs"), "fn main() {}\n").unwrap();
        fs::create_dir_all(root.join("someboot/src")).unwrap();
        fs::write(
            root.join("someboot/Cargo.toml"),
            "[package]\nname = \"someboot\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        fs::write(root.join("someboot/src/lib.rs"), "pub fn marker() {}\n").unwrap();
        fs::write(
            root.join("someboot/build-info.toml"),
            "[x86_64-unknown-none]\ncargoargs = [\"--someboot-cargoarg\"]\nrustflags = [\"-Cdebuginfo=2\"]\n",
        )
        .unwrap();
    }

    #[test]
    fn select_executable_artifact_uses_explicit_bin_first() {
        let artifacts = vec![
            ("kernel".to_string(), artifact("kernel")),
            ("kernel-qemu".to_string(), artifact("kernel-qemu")),
        ];

        let selected = select(&artifacts, Some("kernel-qemu"), None, "kernel").unwrap();

        assert_eq!(
            selected.elf_path,
            Path::new("/tmp/ostool-target/debug/kernel-qemu")
        );
    }

    #[test]
    fn select_executable_artifact_errors_when_explicit_bin_was_not_built() {
        let artifacts = vec![("kernel".to_string(), artifact("kernel"))];

        let err = select(&artifacts, Some("missing-bin"), None, "kernel").unwrap_err();

        assert!(
            err.to_string()
                .contains("binary target `missing-bin` was not built")
        );
    }

    #[test]
    fn select_executable_artifact_prefers_package_name_before_default_run() {
        let artifacts = vec![
            ("helper".to_string(), artifact("helper")),
            ("kernel".to_string(), artifact("kernel")),
        ];

        let selected = select(&artifacts, None, Some("helper"), "kernel").unwrap();

        assert_eq!(
            selected.elf_path,
            Path::new("/tmp/ostool-target/debug/kernel")
        );
    }

    #[test]
    fn select_executable_artifact_uses_default_run_without_package_name_binary() {
        let artifacts = vec![
            ("helper".to_string(), artifact("helper")),
            ("boot-test".to_string(), artifact("boot-test")),
        ];

        let selected = select(&artifacts, None, Some("boot-test"), "kernel").unwrap();

        assert_eq!(
            selected.elf_path,
            Path::new("/tmp/ostool-target/debug/boot-test")
        );
    }

    #[test]
    fn select_executable_artifact_uses_single_binary_as_fallback() {
        let artifacts = vec![("helper".to_string(), artifact("helper"))];

        let selected = select(&artifacts, None, None, "kernel").unwrap();

        assert_eq!(
            selected.elf_path,
            Path::new("/tmp/ostool-target/debug/helper")
        );
    }

    #[test]
    fn select_executable_artifact_errors_on_empty_cargo_output() {
        let err = select(&[], None, None, "kernel").unwrap_err();

        assert!(err.to_string().contains("no executable bin artifact found"));
    }

    #[test]
    fn select_executable_artifact_errors_on_ambiguous_multiple_binaries() {
        let artifacts = vec![
            ("kernel-qemu".to_string(), artifact("kernel-qemu")),
            ("kernel-uboot".to_string(), artifact("kernel-uboot")),
        ];

        let err = select(&artifacts, None, None, "kernel").unwrap_err();

        let rendered = err.to_string();
        assert!(rendered.contains("multiple binary targets"));
        assert!(rendered.contains("kernel-qemu"));
        assert!(rendered.contains("kernel-uboot"));
    }

    #[tokio::test]
    async fn build_cargo_command_skips_someboot_args_when_cargo_config_disables_them() {
        let temp = tempfile::tempdir().unwrap();
        write_someboot_workspace(temp.path());

        let config = Cargo {
            package: "app".into(),
            target: "x86_64-unknown-none".into(),
            disable_someboot_build_config: true,
            profile: Some(CargoBuildProfile::Debug),
            ..Default::default()
        };

        let mut tool = Tool::new(ToolConfig {
            manifest: Some(temp.path().to_path_buf()),
            ..Default::default()
        })
        .unwrap();
        let mut builder = CargoBuilder::build(&mut tool, &config, None).skip_objcopy(true);
        let cmd = builder.build_cargo_command().await.unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert!(!args.iter().any(|arg| arg == "--someboot-cargoarg"));
        assert!(
            !args
                .iter()
                .any(|arg| arg.contains("target.x86_64-unknown-none.rustflags"))
        );
    }

    /// Verifies resolved Cargo artifacts are recorded into runtime state.
    ///
    /// This covers post-resolution Tool state, not serde/config loading.
    #[tokio::test]
    async fn handle_output_records_runtime_artifact_state_without_objcopy() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"kernel\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        fs::create_dir_all(temp.path().join("src")).unwrap();
        fs::write(temp.path().join("src/main.rs"), "fn main() {}\n").unwrap();

        let cargo_artifact_dir = temp.path().join("target/aarch64/debug");
        fs::create_dir_all(&cargo_artifact_dir).unwrap();
        let elf_path = cargo_artifact_dir.join("kernel");
        fs::copy(std::env::current_exe().unwrap(), &elf_path).unwrap();

        let config = Cargo {
            env: HashMap::new(),
            target: "aarch64-unknown-none".into(),
            package: "kernel".into(),
            bin: None,
            features: vec![],
            log: None,
            extra_config: None,
            profile: Some(CargoBuildProfile::Debug),
            disable_someboot_build_config: false,
            args: vec![],
            pre_build_cmds: vec![],
            post_build_cmds: vec![],
            to_bin: true,
        };

        let mut tool = Tool::new(ToolConfig {
            manifest: Some(temp.path().to_path_buf()),
            ..Default::default()
        })
        .unwrap();

        let mut builder = CargoBuilder::build(&mut tool, &config, None).skip_objcopy(true);
        builder.resolved_artifact = Some(ResolvedCargoArtifact {
            elf_path: elf_path.clone(),
            cargo_artifact_dir: cargo_artifact_dir.clone(),
        });
        builder.handle_output().await.unwrap();
        drop(builder);

        let expected_elf = elf_path.canonicalize().unwrap();
        assert_eq!(tool.ctx.artifacts.elf.as_ref(), Some(&expected_elf));
        assert!(tool.ctx.artifacts.bin.is_none());
        assert_eq!(
            tool.ctx.artifacts.cargo_artifact_dir.as_ref(),
            Some(&cargo_artifact_dir)
        );
        assert_eq!(
            tool.ctx.artifacts.runtime_artifact_dir.as_ref(),
            Some(&cargo_artifact_dir)
        );
        assert!(tool.ctx.arch.is_some());
    }
}
