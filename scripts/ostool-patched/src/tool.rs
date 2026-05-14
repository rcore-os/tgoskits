use std::{
    env::current_dir,
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use anyhow::{Context, anyhow, bail};
use cargo_metadata::Metadata;
use colored::Colorize;
use jkconfig::data::{
    ElementHook, HookContext, HookFlow, HookOption, MessageLevel, MultiSelectBinding,
    MultiSelectSpec, SingleSelectBinding, SingleSelectSpec,
};
use object::Object;
use tokio::fs;

use crate::{
    build::{
        config::{BuildConfig, BuildSystem, Cargo},
        someboot,
    },
    ctx::AppContext,
    utils::{PathResultExt, replace_placeholders},
};

/// Static configuration used to initialize a [`Tool`].
#[derive(Default, Clone, Debug)]
pub struct ToolConfig {
    /// Optional manifest path or manifest directory.
    pub manifest: Option<PathBuf>,
    /// Optional custom build output directory.
    pub build_dir: Option<PathBuf>,
    /// Optional custom binary output directory.
    pub bin_dir: Option<PathBuf>,
    /// Whether debug mode is enabled.
    pub debug: bool,
}

/// Main library object orchestrating build and run operations.
#[derive(Clone, Debug)]
pub struct Tool {
    pub(crate) config: ToolConfig,
    pub(crate) manifest_path: PathBuf,
    pub(crate) manifest_dir: PathBuf,
    pub(crate) workspace_dir: PathBuf,
    pub(crate) ctx: AppContext,
}

/// Resolved Cargo manifest and workspace paths derived from `cargo metadata`.
#[derive(Clone, Debug)]
pub struct ManifestContext {
    pub manifest_path: PathBuf,
    pub manifest_dir: PathBuf,
    pub workspace_dir: PathBuf,
}

impl Tool {
    /// Creates a new tool from the provided configuration.
    pub fn new(config: ToolConfig) -> anyhow::Result<Self> {
        let manifest = resolve_manifest_context(config.manifest.clone())?;

        Ok(Self {
            config,
            manifest_path: manifest.manifest_path,
            manifest_dir: manifest.manifest_dir,
            workspace_dir: manifest.workspace_dir,
            ctx: AppContext::default(),
        })
    }

    pub fn ctx(&self) -> &AppContext {
        &self.ctx
    }

    pub fn ctx_mut(&mut self) -> &mut AppContext {
        &mut self.ctx
    }

    pub fn set_build_config_path(&mut self, path: Option<PathBuf>) {
        self.ctx.build_config_path = path;
    }

    pub fn into_context(self) -> AppContext {
        self.ctx
    }

    pub(crate) fn debug_enabled(&self) -> bool {
        self.config.debug
    }

    pub(crate) fn sync_cargo_context(&mut self, cargo: &Cargo) {
        self.ctx.build_config = Some(BuildConfig {
            system: BuildSystem::Cargo(cargo.clone()),
        });
    }

    pub(crate) fn manifest_dir(&self) -> &PathBuf {
        &self.manifest_dir
    }

    pub(crate) fn workspace_dir(&self) -> &PathBuf {
        &self.workspace_dir
    }

    pub(crate) fn build_dir(&self) -> PathBuf {
        self.config
            .build_dir
            .as_ref()
            .map(|dir| self.resolve_dir(dir))
            .unwrap_or_else(|| self.manifest_dir.join("target"))
    }

    pub(crate) fn bin_dir(&self) -> Option<PathBuf> {
        self.config
            .bin_dir
            .as_ref()
            .map(|dir| self.resolve_dir(dir))
    }

    fn resolve_dir(&self, dir: &PathBuf) -> PathBuf {
        if dir.is_relative() {
            self.manifest_dir.join(dir)
        } else {
            dir.clone()
        }
    }

    /// Executes a shell command in the current context.
    pub(crate) fn shell_run_cmd(&self, cmd: &str) -> anyhow::Result<()> {
        let mut command = match std::env::consts::OS {
            "windows" => {
                let mut command = self.command("powershell");
                command.arg("-Command");
                command
            }
            _ => {
                let mut command = self.command("sh");
                command.arg("-c");
                command
            }
        };

        command.arg(cmd);

        if let Some(elf) = &self.ctx.artifacts.elf {
            command.env("KERNEL_ELF", elf.display().to_string());
        }

        command.run()?;
        Ok(())
    }

    /// Creates a new command builder for the given program.
    pub(crate) fn command(&self, program: &str) -> crate::utils::Command {
        let tool = self.clone();
        let mut command =
            crate::utils::Command::new(program, &self.manifest_dir, move |s| tool.replace_value(s));
        command.env("WORKSPACE_FOLDER", self.workspace_dir.display().to_string());
        command
    }

    /// Gets the Cargo metadata for the current manifest.
    pub fn metadata(&self) -> anyhow::Result<Metadata> {
        cargo_metadata::MetadataCommand::new()
            .manifest_path(&self.manifest_path)
            .no_deps()
            .exec()
            .with_context(|| {
                format!(
                    "failed to load cargo metadata from {}",
                    self.manifest_path.display()
                )
            })
    }

    pub(crate) fn resolve_package_manifest_dir(&self, package: &str) -> anyhow::Result<PathBuf> {
        let metadata = self.metadata()?;
        let Some(pkg) = metadata.packages.iter().find(|pkg| pkg.name == package) else {
            bail!(
                "package '{}' not found in cargo metadata under {}",
                package,
                self.manifest_dir().display()
            );
        };

        pkg.manifest_path
            .parent()
            .map(|path| path.as_std_path().to_path_buf())
            .ok_or_else(|| {
                anyhow!(
                    "package '{}' manifest has no parent: {}",
                    package,
                    pkg.manifest_path
                )
            })
    }

    /// Sets the ELF artifact path and synchronizes derived runtime metadata.
    pub(crate) async fn set_elf_artifact_path(&mut self, path: PathBuf) -> anyhow::Result<()> {
        let path = path
            .canonicalize()
            .with_path("failed to canonicalize file", &path)?;
        let artifact_dir = path
            .parent()
            .ok_or_else(|| anyhow!("invalid ELF file path: {}", path.display()))?
            .to_path_buf();

        self.ctx.artifacts.elf = Some(path.clone());
        self.ctx.artifacts.bin = None;
        self.ctx.artifacts.cargo_artifact_dir = Some(artifact_dir.clone());
        self.ctx.artifacts.runtime_artifact_dir = Some(artifact_dir);

        let binary_data = fs::read(&path)
            .await
            .with_path("failed to read ELF file", &path)?;
        let file = object::File::parse(binary_data.as_slice())
            .with_context(|| format!("failed to parse ELF file: {}", path.display()))?;
        self.ctx.arch = Some(file.architecture());
        Ok(())
    }

    /// Imports an ELF artifact, strips it to a runtime `.elf`, and optionally
    /// materializes a `.bin` image.
    pub async fn prepare_elf_artifact(
        &mut self,
        path: PathBuf,
        to_bin: bool,
    ) -> anyhow::Result<()> {
        self.set_elf_artifact_path(path).await?;
        self.objcopy_elf()?;
        if to_bin {
            self.objcopy_output_bin()?;
        }
        Ok(())
    }

    /// Strips debug symbols from the ELF file.
    pub(crate) fn objcopy_elf(&mut self) -> anyhow::Result<PathBuf> {
        let elf_path = self
            .ctx
            .artifacts
            .elf
            .as_ref()
            .ok_or_else(|| anyhow!("elf not exist"))?;
        let elf_path = elf_path
            .canonicalize()
            .with_path("failed to canonicalize file", elf_path)?;

        let stripped_elf_path = elf_path.with_file_name(
            elf_path
                .file_stem()
                .ok_or_else(|| anyhow!("invalid ELF file path: {}", elf_path.display()))?
                .to_string_lossy()
                .to_string()
                + ".elf",
        );
        println!(
            "{}",
            format!(
                "Stripping ELF file...\r\n  original elf: {}\r\n  stripped elf: {}",
                elf_path.display(),
                stripped_elf_path.display()
            )
            .bold()
            .purple()
        );

        let mut objcopy = self.command("rust-objcopy");
        objcopy.arg(format!(
            "--binary-architecture={}",
            format!(
                "{:?}",
                self.ctx
                    .arch
                    .ok_or_else(|| anyhow!("architecture not detected"))?
            )
            .to_lowercase()
        ));
        objcopy.arg(&elf_path);
        objcopy.arg(&stripped_elf_path);
        objcopy.run()?;

        self.ctx.artifacts.elf = Some(stripped_elf_path.clone());
        self.ctx.artifacts.bin = None;
        self.ctx.artifacts.cargo_artifact_dir = stripped_elf_path.parent().map(PathBuf::from);
        self.ctx.artifacts.runtime_artifact_dir = stripped_elf_path.parent().map(PathBuf::from);

        Ok(stripped_elf_path)
    }

    /// Converts the ELF file to raw binary format.
    pub(crate) fn objcopy_output_bin(&mut self) -> anyhow::Result<PathBuf> {
        if let Some(bin) = &self.ctx.artifacts.bin {
            debug!("BIN file already exists: {:?}", bin);
            return Ok(bin.clone());
        }

        let elf_path = self
            .ctx
            .artifacts
            .elf
            .as_ref()
            .ok_or_else(|| anyhow!("elf not exist"))?;
        let elf_path = elf_path
            .canonicalize()
            .with_path("failed to canonicalize file", elf_path)?;

        let bin_name = elf_path
            .file_stem()
            .ok_or_else(|| anyhow!("invalid ELF file path: {}", elf_path.display()))?
            .to_string_lossy()
            .to_string()
            + ".bin";

        let bin_path = if let Some(bin_dir) = self.bin_dir() {
            bin_dir.join(bin_name)
        } else {
            elf_path.with_file_name(bin_name)
        };

        if let Some(parent) = bin_path.parent() {
            std::fs::create_dir_all(parent).with_path("failed to create directory", parent)?;
        }

        println!(
            "{}",
            format!(
                "Converting ELF to BIN format...\r\n  elf: {}\r\n  bin: {}",
                elf_path.display(),
                bin_path.display()
            )
            .bold()
            .purple()
        );

        let mut objcopy = self.command("rust-objcopy");

        if !self.debug_enabled() {
            objcopy.arg("--strip-all");
        }

        objcopy
            .arg("-O")
            .arg("binary")
            .arg(&elf_path)
            .arg(&bin_path);
        objcopy.run()?;

        self.ctx.artifacts.bin = Some(bin_path.clone());
        self.ctx.artifacts.runtime_artifact_dir = bin_path.parent().map(PathBuf::from);
        Ok(bin_path)
    }

    pub(crate) fn resolve_build_config_path(&self, explicit_path: Option<PathBuf>) -> PathBuf {
        explicit_path.unwrap_or_else(|| self.workspace_dir.join(".build.toml"))
    }

    /// Loads and prepares the build configuration.
    pub(crate) async fn prepare_build_config(
        &mut self,
        config_path: Option<PathBuf>,
        menu: bool,
    ) -> anyhow::Result<BuildConfig> {
        let config_path = self.resolve_build_config_path(config_path);
        self.ctx.build_config_path = Some(config_path.clone());

        let hooks = self.ui_hooks();
        let Some(mut c): Option<BuildConfig> = jkconfig::run(config_path.clone(), menu, &hooks)
            .await
            .with_context(|| format!("failed to load build config: {}", config_path.display()))?
        else {
            bail!("No build configuration obtained");
        };

        if let BuildSystem::Cargo(cargo) = &mut c.system {
            let iter = self.someboot_cargo_args(cargo)?.into_iter();
            cargo.args.extend(iter);
        }

        self.ctx.build_config = Some(c.clone());
        Ok(c)
    }

    fn someboot_cargo_args(&self, cargo: &Cargo) -> anyhow::Result<Vec<String>> {
        let manifest_path = self.workspace_dir.join("Cargo.toml");
        someboot::detect_build_config_for_package(
            &manifest_path,
            &cargo.package,
            &cargo.features,
            &cargo.target,
        )
    }

    pub(crate) fn replace_value<S>(&self, value: S) -> String
    where
        S: AsRef<OsStr>,
    {
        self.replace_string(&value.as_ref().to_string_lossy())
            .unwrap_or_else(|_| value.as_ref().to_string_lossy().into_owned())
    }

    pub(crate) fn replace_string(&self, input: &str) -> anyhow::Result<String> {
        let package_dir = self.package_root_for_variables()?;
        let workspace_dir = self.workspace_dir.display().to_string();
        let package_dir = package_dir.display().to_string();
        let tmp_dir = std::env::temp_dir().display().to_string();

        replace_placeholders(input, |placeholder| {
            let value = match placeholder {
                "workspace" | "workspaceFolder" => Some(workspace_dir.clone()),
                "package" => Some(package_dir.clone()),
                "tmpDir" => Some(tmp_dir.clone()),
                p if p.starts_with("env:") => Some(std::env::var(&p[4..]).unwrap_or_default()),
                _ => None,
            };
            Ok(value)
        })
    }

    pub(crate) fn replace_path_variables(&self, path: PathBuf) -> anyhow::Result<PathBuf> {
        Ok(PathBuf::from(self.replace_string(&path.to_string_lossy())?))
    }

    fn package_root_for_variables(&self) -> anyhow::Result<PathBuf> {
        if let Some(BuildConfig {
            system: BuildSystem::Cargo(cargo),
        }) = &self.ctx.build_config
        {
            return self.resolve_package_manifest_dir(&cargo.package);
        }

        Ok(self.manifest_dir.clone())
    }

    pub(crate) fn ui_hooks(&self) -> Vec<ElementHook> {
        vec![
            self.ui_hook_feature_select(),
            self.ui_hook_package_select(),
            self.ui_hook_target_select(),
        ]
    }

    fn ui_hook_feature_select(&self) -> ElementHook {
        let path = "system.features";
        let cargo_toml = self.workspace_dir.join("Cargo.toml");
        ElementHook {
            path: path.into(),
            callback: Arc::new(move |ctx: &mut HookContext<'_>, path| {
                let package = ctx
                    .get_string("system.package")?
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if package.is_empty() {
                    ctx.show_message(
                        jkconfig::data::MessageLevel::Warning,
                        "Select a package before editing features.",
                    );
                    return Ok(HookFlow::Consumed);
                }

                let feature_options = collect_feature_options(&cargo_toml, &package, None)?;
                let options = feature_options
                    .into_iter()
                    .map(|feature| HookOption::new(feature.clone(), feature))
                    .collect();

                ctx.present_multi_select(MultiSelectSpec {
                    title: format!("Features for {package}"),
                    help: Some(
                        "Space toggle  Enter apply. Dependency features use dep_name/feature."
                            .into(),
                    ),
                    options,
                    selected: ctx.get_strings(path.clone())?,
                    min_selected: None,
                    max_selected: None,
                    binding: MultiSelectBinding::SetStringArray { path: path.clone() },
                })?;

                Ok(HookFlow::Consumed)
            }),
        }
    }

    fn ui_hook_package_select(&self) -> ElementHook {
        let path = "system.package";
        let cargo_toml = self.workspace_dir.join("Cargo.toml");

        ElementHook {
            path: path.into(),
            callback: Arc::new(move |ctx: &mut HookContext<'_>, path| {
                let mut items = Vec::new();
                if let Ok(metadata) = cargo_metadata::MetadataCommand::new()
                    .manifest_path(&cargo_toml)
                    .no_deps()
                    .exec()
                {
                    for pkg in &metadata.packages {
                        items.push(pkg.name.to_string());
                    }
                }

                let options = items
                    .into_iter()
                    .map(|item| HookOption::new(item.clone(), item))
                    .collect();
                ctx.present_single_select(SingleSelectSpec {
                    title: "Select Package".into(),
                    help: Some("Choose the Cargo package used by the build config.".into()),
                    options,
                    initial: ctx.get_string(path.clone())?,
                    allow_clear: false,
                    binding: SingleSelectBinding::SetString { path: path.clone() },
                })?;
                Ok(HookFlow::Consumed)
            }),
        }
    }

    fn ui_hook_target_select(&self) -> ElementHook {
        let path = "system.target";
        let cargo_toml = self.workspace_dir.join("Cargo.toml");

        ElementHook {
            path: path.into(),
            callback: Arc::new(move |ctx: &mut HookContext<'_>, path| {
                let package = ctx
                    .get_string("system.package")?
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                let current_target = ctx.get_string(path.clone())?;

                let mut warnings = Vec::new();
                let (options, help) = if package.is_empty() {
                    fallback_rustup_targets()?
                } else {
                    match collect_package_doc_targets(&cargo_toml, &package) {
                        Ok(Some(doc_targets)) => (
                            build_target_options(TargetCandidateSet::DocsRs(&doc_targets)),
                            "Select a target declared by the selected package docs.rs metadata."
                                .to_string(),
                        ),
                        Ok(None) => fallback_rustup_targets()?,
                        Err(err) => {
                            warnings.push(format!(
                                "Failed to inspect docs.rs targets for package '{package}': {err}"
                            ));
                            fallback_rustup_targets()?
                        }
                    }
                };

                if options.is_empty() {
                    bail!("No target candidates available for selection");
                }

                for warning in warnings {
                    ctx.show_message(MessageLevel::Warning, warning);
                }

                ctx.present_single_select(SingleSelectSpec {
                    title: "Select Target".into(),
                    help: Some(help),
                    options,
                    initial: current_target,
                    allow_clear: false,
                    binding: SingleSelectBinding::SetString { path: path.clone() },
                })?;

                Ok(HookFlow::Consumed)
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RustupTargetOption {
    triple: String,
    installed: bool,
}

enum TargetCandidateSet<'a> {
    DocsRs(&'a [String]),
    Rustup(&'a [RustupTargetOption]),
}

fn fallback_rustup_targets() -> anyhow::Result<(Vec<HookOption>, String)> {
    let rustup_targets = collect_rustup_targets()?;
    if rustup_targets.is_empty() {
        bail!("No Rust targets available from `rustup target list`");
    }
    Ok((
        build_target_options(TargetCandidateSet::Rustup(&rustup_targets)),
        "Package has no docs.rs targets; showing rustup targets.".to_string(),
    ))
}

fn collect_feature_options(
    manifest_path: &Path,
    package_name: &str,
    deps_filter: Option<&[String]>,
) -> anyhow::Result<Vec<String>> {
    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(manifest_path)
        .no_deps()
        .exec()?;
    let Some(pkg) = metadata
        .packages
        .iter()
        .find(|pkg| pkg.name == package_name)
    else {
        bail!(
            "package '{package_name}' not found in {}",
            manifest_path.display()
        );
    };

    let mut features = pkg.features.keys().cloned().collect::<Vec<_>>();
    features.sort();

    for dependency in &pkg.dependencies {
        let include = match deps_filter {
            Some(filter) => filter.contains(&dependency.name),
            None => true,
        };
        if !include {
            continue;
        }

        let Some(dep_pkg) = metadata
            .packages
            .iter()
            .find(|candidate| candidate.name == dependency.name)
        else {
            continue;
        };
        let mut dep_features = dep_pkg.features.keys().cloned().collect::<Vec<_>>();
        dep_features.sort();
        features.extend(
            dep_features
                .into_iter()
                .map(|feature| format!("{}/{}", dependency.name, feature)),
        );
    }

    Ok(features)
}

fn collect_package_doc_targets(
    manifest_path: &Path,
    package_name: &str,
) -> anyhow::Result<Option<Vec<String>>> {
    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(manifest_path)
        .no_deps()
        .exec()
        .with_context(|| {
            format!(
                "failed to load cargo metadata from {}",
                manifest_path.display()
            )
        })?;
    let Some(pkg) = metadata
        .packages
        .iter()
        .find(|pkg| pkg.name == package_name)
    else {
        bail!(
            "package '{package_name}' not found in {}",
            manifest_path.display()
        );
    };

    parse_docs_rs_targets(&pkg.metadata)
}

fn parse_docs_rs_targets(metadata: &serde_json::Value) -> anyhow::Result<Option<Vec<String>>> {
    let Some(docs) = metadata.get("docs") else {
        return Ok(None);
    };
    let Some(docs_rs) = docs.get("rs") else {
        return Ok(None);
    };

    let targets = match docs_rs.get("targets") {
        Some(serde_json::Value::Array(values)) => {
            let mut targets = Vec::with_capacity(values.len());
            for value in values {
                let target = value.as_str().ok_or_else(|| {
                    anyhow!("package.metadata.docs.rs.targets must be an array of strings")
                })?;
                let target = target.trim();
                if target.is_empty() {
                    bail!("package.metadata.docs.rs.targets must not contain empty strings");
                }
                if !targets.iter().any(|existing| existing == target) {
                    targets.push(target.to_string());
                }
            }
            Some(targets)
        }
        Some(_) => bail!("package.metadata.docs.rs.targets must be an array of strings"),
        None => None,
    };

    let default_target = match docs_rs.get("default-target") {
        Some(serde_json::Value::String(value)) => {
            let value = value.trim();
            if value.is_empty() {
                bail!("package.metadata.docs.rs.default-target must not be empty");
            }
            Some(value.to_string())
        }
        Some(_) => bail!("package.metadata.docs.rs.default-target must be a string"),
        None => None,
    };

    let mut normalized = match targets {
        Some(targets) if !targets.is_empty() => targets,
        _ => Vec::new(),
    };

    if let Some(default_target) = default_target {
        if let Some(index) = normalized
            .iter()
            .position(|target| target == &default_target)
        {
            let value = normalized.remove(index);
            normalized.insert(0, value);
        } else {
            normalized.insert(0, default_target);
        }
    }

    if normalized.is_empty() {
        Ok(None)
    } else {
        Ok(Some(normalized))
    }
}

fn collect_rustup_targets() -> anyhow::Result<Vec<RustupTargetOption>> {
    let output = Command::new("rustup")
        .args(["target", "list"])
        .output()
        .context("failed to run `rustup target list`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "`rustup target list` failed with {}:\n{}",
            output.status,
            stderr.trim()
        );
    }

    let stdout = String::from_utf8(output.stdout)
        .context("`rustup target list` output is not valid UTF-8")?;
    Ok(parse_rustup_targets(&stdout))
}

fn parse_rustup_targets(output: &str) -> Vec<RustupTargetOption> {
    let mut installed = Vec::new();
    let mut available = Vec::new();

    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let installed_flag = line.ends_with(" (installed)");
        let triple = line
            .strip_suffix(" (installed)")
            .unwrap_or(line)
            .trim()
            .to_string();
        if triple.is_empty() {
            continue;
        }

        let option = RustupTargetOption {
            triple,
            installed: installed_flag,
        };
        if installed_flag {
            installed.push(option);
        } else {
            available.push(option);
        }
    }

    installed.extend(available);
    installed
}

fn build_target_options(candidates: TargetCandidateSet<'_>) -> Vec<HookOption> {
    match candidates {
        TargetCandidateSet::DocsRs(targets) => targets
            .iter()
            .cloned()
            .map(|target| HookOption {
                value: target.clone(),
                label: target,
                detail: Some("docs.rs target".into()),
                disabled: false,
            })
            .collect(),
        TargetCandidateSet::Rustup(targets) => targets
            .iter()
            .map(|target| HookOption {
                value: target.triple.clone(),
                label: target.triple.clone(),
                detail: Some(if target.installed {
                    "installed".into()
                } else {
                    "available".into()
                }),
                disabled: false,
            })
            .collect(),
    }
}

pub fn resolve_manifest_context(input: Option<PathBuf>) -> anyhow::Result<ManifestContext> {
    let manifest_path = resolve_manifest_path(input)?;
    let manifest_dir = manifest_path
        .parent()
        .ok_or_else(|| anyhow!("manifest has no parent: {}", manifest_path.display()))?
        .to_path_buf();

    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(&manifest_path)
        .no_deps()
        .exec()
        .with_context(|| {
            format!(
                "failed to load cargo metadata from {}",
                manifest_path.display()
            )
        })?;

    Ok(ManifestContext {
        manifest_path,
        manifest_dir,
        workspace_dir: PathBuf::from(metadata.workspace_root.as_std_path()),
    })
}

fn resolve_manifest_path(input: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    let path = match input {
        Some(path) => path,
        None => current_dir().context("failed to get current working directory")?,
    };

    let manifest_path = if path.is_dir() {
        path.join("Cargo.toml")
    } else {
        path
    };

    if manifest_path.file_name().and_then(|name| name.to_str()) != Some("Cargo.toml") {
        bail!(
            "manifest must be a Cargo.toml file or a directory containing Cargo.toml: {}",
            manifest_path.display()
        );
    }

    if !manifest_path.exists() {
        bail!("Cargo.toml not found: {}", manifest_path.display());
    }

    manifest_path
        .canonicalize()
        .with_path("failed to canonicalize manifest path", &manifest_path)
}

#[cfg(test)]
mod tests {
    use super::{
        Tool, ToolConfig, build_target_options, collect_package_doc_targets, parse_rustup_targets,
        resolve_manifest_context,
    };
    use crate::build::config::{BuildConfig, BuildSystem, Cargo};
    use crate::run::qemu::resolve_qemu_config_path_in_dir;
    use jkconfig::data::ElementHook;
    use object::Architecture;
    use std::{
        collections::HashMap,
        fs,
        path::{Path, PathBuf},
    };

    #[tokio::test]
    async fn set_elf_artifact_path_updates_dirs_and_arch() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"sample\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("src/lib.rs"), "").unwrap();

        let source = std::env::current_exe().unwrap();
        let copied = temp.path().join("sample-elf");
        std::fs::copy(&source, &copied).unwrap();

        let mut tool = Tool::new(ToolConfig {
            manifest: Some(temp.path().to_path_buf()),
            ..Default::default()
        })
        .unwrap();
        tool.set_elf_artifact_path(copied.clone()).await.unwrap();

        let expected_elf = copied.canonicalize().unwrap();
        let expected_dir = expected_elf.parent().unwrap().to_path_buf();

        assert_eq!(tool.ctx.artifacts.elf.as_ref(), Some(&expected_elf));
        assert_eq!(
            tool.ctx.artifacts.cargo_artifact_dir.as_ref(),
            Some(&expected_dir)
        );
        assert_eq!(
            tool.ctx.artifacts.runtime_artifact_dir.as_ref(),
            Some(&expected_dir)
        );
        assert!(tool.ctx.arch.is_some());
        assert!(tool.ctx.artifacts.bin.is_none());
    }

    #[test]
    fn resolve_manifest_context_uses_workspace_root() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\"]\nresolver = \"3\"\n",
        )
        .unwrap();

        let app_dir = temp.path().join("app");
        std::fs::create_dir_all(app_dir.join("src")).unwrap();
        std::fs::write(
            app_dir.join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::write(app_dir.join("src/main.rs"), "fn main() {}\n").unwrap();

        let manifest = resolve_manifest_context(Some(app_dir.clone())).unwrap();

        assert_eq!(manifest.manifest_path, app_dir.join("Cargo.toml"));
        assert_eq!(manifest.manifest_dir, app_dir);
        assert_eq!(manifest.workspace_dir, temp.path());
    }

    #[test]
    fn resolve_package_manifest_dir_uses_selected_package() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\", \"kernel\"]\nresolver = \"3\"\n",
        )
        .unwrap();

        let app_dir = temp.path().join("app");
        std::fs::create_dir_all(app_dir.join("src")).unwrap();
        std::fs::write(
            app_dir.join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::write(app_dir.join("src/main.rs"), "fn main() {}\n").unwrap();

        let kernel_dir = temp.path().join("kernel");
        std::fs::create_dir_all(kernel_dir.join("src")).unwrap();
        std::fs::write(
            kernel_dir.join("Cargo.toml"),
            "[package]\nname = \"kernel\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::write(kernel_dir.join("src/main.rs"), "fn main() {}\n").unwrap();

        let tool = Tool::new(ToolConfig {
            manifest: Some(app_dir.clone()),
            ..Default::default()
        })
        .unwrap();

        let resolved = tool.resolve_package_manifest_dir("kernel").unwrap();
        assert_eq!(resolved, kernel_dir);
    }

    #[test]
    fn cargo_qemu_config_resolution_prefers_package_dir_over_workspace_root() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\", \"kernel\"]\nresolver = \"3\"\n",
        )
        .unwrap();
        std::fs::write(temp.path().join("qemu-aarch64.toml"), "").unwrap();

        let app_dir = temp.path().join("app");
        std::fs::create_dir_all(app_dir.join("src")).unwrap();
        std::fs::write(
            app_dir.join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::write(app_dir.join("src/main.rs"), "fn main() {}\n").unwrap();

        let kernel_dir = temp.path().join("kernel");
        std::fs::create_dir_all(kernel_dir.join("src")).unwrap();
        std::fs::write(
            kernel_dir.join("Cargo.toml"),
            "[package]\nname = \"kernel\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::write(kernel_dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(kernel_dir.join(".qemu-aarch64.toml"), "").unwrap();

        let tool = Tool::new(ToolConfig {
            manifest: Some(app_dir),
            ..Default::default()
        })
        .unwrap();

        let package_dir = tool.resolve_package_manifest_dir("kernel").unwrap();
        let resolved =
            resolve_qemu_config_path_in_dir(&package_dir, Some(Architecture::Aarch64), None)
                .unwrap();

        assert_eq!(resolved, kernel_dir.join(".qemu-aarch64.toml"));
    }

    #[test]
    fn replace_string_uses_workspace_and_legacy_workspacefolder() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"sample\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("src/lib.rs"), "").unwrap();

        let tool = Tool::new(ToolConfig {
            manifest: Some(temp.path().to_path_buf()),
            ..Default::default()
        })
        .unwrap();

        let replaced = tool
            .replace_string("${workspace}:${workspaceFolder}")
            .unwrap();
        let expected = temp.path().display().to_string();
        assert_eq!(replaced, format!("{expected}:{expected}"));
    }

    #[test]
    fn replace_string_uses_cross_platform_tmpdir() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"sample\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("src/lib.rs"), "").unwrap();

        let tool = Tool::new(ToolConfig {
            manifest: Some(temp.path().to_path_buf()),
            ..Default::default()
        })
        .unwrap();

        let replaced = tool.replace_string("${tmpDir}").unwrap();
        assert_eq!(replaced, std::env::temp_dir().display().to_string());
    }

    #[test]
    fn replace_string_uses_package_dir_from_build_config() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\", \"kernel\"]\nresolver = \"3\"\n",
        )
        .unwrap();

        let app_dir = temp.path().join("app");
        std::fs::create_dir_all(app_dir.join("src")).unwrap();
        std::fs::write(
            app_dir.join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::write(app_dir.join("src/main.rs"), "fn main() {}\n").unwrap();

        let kernel_dir = temp.path().join("kernel");
        std::fs::create_dir_all(kernel_dir.join("src")).unwrap();
        std::fs::write(
            kernel_dir.join("Cargo.toml"),
            "[package]\nname = \"kernel\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::write(kernel_dir.join("src/main.rs"), "fn main() {}\n").unwrap();

        let mut tool = Tool::new(ToolConfig {
            manifest: Some(app_dir),
            ..Default::default()
        })
        .unwrap();
        tool.ctx.build_config = Some(BuildConfig {
            system: BuildSystem::Cargo(Cargo {
                env: HashMap::new(),
                target: "aarch64-unknown-none".into(),
                package: "kernel".into(),
                features: vec![],
                log: None,
                extra_config: None,
                args: vec![],
                pre_build_cmds: vec![],
                post_build_cmds: vec![],
                to_bin: false,
            }),
        });

        let replaced = tool.replace_string("${package}").unwrap();
        assert_eq!(replaced, kernel_dir.display().to_string());
    }

    #[test]
    fn replace_string_falls_back_to_manifest_dir_for_package() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"sample\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("src/lib.rs"), "").unwrap();

        let tool = Tool::new(ToolConfig {
            manifest: Some(temp.path().to_path_buf()),
            ..Default::default()
        })
        .unwrap();

        let replaced = tool.replace_string("${package}").unwrap();
        assert_eq!(replaced, temp.path().display().to_string());
    }

    #[test]
    fn command_replaces_args_and_env() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"sample\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("src/lib.rs"), "").unwrap();

        let tool = Tool::new(ToolConfig {
            manifest: Some(temp.path().to_path_buf()),
            ..Default::default()
        })
        .unwrap();

        let mut cmd = tool.command("echo");
        cmd.arg("${workspace}");
        cmd.env("PKG_DIR", "${package}");

        let args: Vec<String> = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, vec![temp.path().display().to_string()]);

        let envs: Vec<(String, String)> = cmd
            .get_envs()
            .filter_map(|(k, v)| {
                Some((
                    k.to_string_lossy().into_owned(),
                    v?.to_string_lossy().into_owned(),
                ))
            })
            .collect();
        assert!(
            envs.iter()
                .any(|(k, v)| k == "PKG_DIR" && v == &temp.path().display().to_string())
        );
    }

    #[test]
    fn collect_package_doc_targets_uses_targets_list() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = write_workspace_with_package(
            temp.path(),
            "kernel",
            Some(
                r#"[package.metadata.docs.rs]
targets = ["riscv64gc-unknown-none-elf", "aarch64-unknown-none"]
"#,
            ),
        );

        let targets = collect_package_doc_targets(&manifest, "kernel")
            .unwrap()
            .unwrap();
        assert_eq!(
            targets,
            vec![
                "riscv64gc-unknown-none-elf".to_string(),
                "aarch64-unknown-none".to_string()
            ]
        );
    }

    #[test]
    fn collect_package_doc_targets_uses_default_target_when_targets_missing() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = write_workspace_with_package(
            temp.path(),
            "kernel",
            Some(
                r#"[package.metadata.docs.rs]
default-target = "aarch64-unknown-none"
"#,
            ),
        );

        let targets = collect_package_doc_targets(&manifest, "kernel")
            .unwrap()
            .unwrap();
        assert_eq!(targets, vec!["aarch64-unknown-none".to_string()]);
    }

    #[test]
    fn collect_package_doc_targets_moves_default_target_to_front() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = write_workspace_with_package(
            temp.path(),
            "kernel",
            Some(
                r#"[package.metadata.docs.rs]
targets = ["x86_64-unknown-none", "aarch64-unknown-none", "x86_64-unknown-none"]
default-target = "aarch64-unknown-none"
"#,
            ),
        );

        let targets = collect_package_doc_targets(&manifest, "kernel")
            .unwrap()
            .unwrap();
        assert_eq!(
            targets,
            vec![
                "aarch64-unknown-none".to_string(),
                "x86_64-unknown-none".to_string()
            ]
        );
    }

    #[test]
    fn collect_package_doc_targets_rejects_invalid_docs_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = write_workspace_with_package(
            temp.path(),
            "kernel",
            Some(
                r#"[package.metadata.docs.rs]
targets = "aarch64-unknown-none"
"#,
            ),
        );

        let err = collect_package_doc_targets(&manifest, "kernel")
            .unwrap_err()
            .to_string();
        assert!(err.contains("targets"));
        assert!(err.contains("array of strings"));
    }

    #[test]
    fn collect_package_doc_targets_errors_for_missing_package() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = write_workspace_with_package(temp.path(), "kernel", None);

        let err = collect_package_doc_targets(&manifest, "missing")
            .unwrap_err()
            .to_string();
        assert!(err.contains("package 'missing' not found"));
    }

    #[test]
    fn parse_rustup_targets_prioritizes_installed_entries() {
        let parsed = parse_rustup_targets(
            "aarch64-unknown-none\nx86_64-unknown-none (installed)\nriscv64gc-unknown-none-elf\nthumbv7em-none-eabihf (installed)\n",
        );

        let triples: Vec<_> = parsed.iter().map(|target| target.triple.as_str()).collect();
        let installed: Vec<_> = parsed.iter().map(|target| target.installed).collect();
        assert_eq!(
            triples,
            vec![
                "x86_64-unknown-none",
                "thumbv7em-none-eabihf",
                "aarch64-unknown-none",
                "riscv64gc-unknown-none-elf"
            ]
        );
        assert_eq!(installed, vec![true, true, false, false]);
    }

    #[test]
    fn parse_rustup_targets_handles_empty_output() {
        let parsed = parse_rustup_targets("");
        assert!(parsed.is_empty());
    }

    #[test]
    fn build_target_options_marks_rustup_install_state() {
        let options = build_target_options(super::TargetCandidateSet::Rustup(&[
            super::RustupTargetOption {
                triple: "x86_64-unknown-none".into(),
                installed: true,
            },
            super::RustupTargetOption {
                triple: "aarch64-unknown-none".into(),
                installed: false,
            },
        ]));
        assert_eq!(options[0].detail.as_deref(), Some("installed"));
        assert_eq!(options[1].detail.as_deref(), Some("available"));
    }

    #[test]
    fn ui_hooks_include_system_target_hook() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"sample\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        fs::create_dir_all(temp.path().join("src")).unwrap();
        fs::write(temp.path().join("src/lib.rs"), "").unwrap();

        let tool = Tool::new(ToolConfig {
            manifest: Some(temp.path().to_path_buf()),
            ..Default::default()
        })
        .unwrap();

        let hooks: Vec<ElementHook> = tool.ui_hooks();
        assert!(
            hooks
                .iter()
                .any(|hook| hook.path.as_key() == "system.target")
        );
    }

    fn write_workspace_with_package(root: &Path, package: &str, metadata: Option<&str>) -> PathBuf {
        fs::write(
            root.join("Cargo.toml"),
            format!("[workspace]\nmembers = [\"{package}\"]\nresolver = \"3\"\n"),
        )
        .unwrap();

        let package_dir = root.join(package);
        fs::create_dir_all(package_dir.join("src")).unwrap();
        let mut cargo_toml =
            format!("[package]\nname = \"{package}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n");
        if let Some(metadata) = metadata {
            cargo_toml.push('\n');
            cargo_toml.push_str(metadata);
        }
        fs::write(package_dir.join("Cargo.toml"), cargo_toml).unwrap();
        fs::write(package_dir.join("src/lib.rs"), "").unwrap();
        root.join("Cargo.toml")
    }
}
