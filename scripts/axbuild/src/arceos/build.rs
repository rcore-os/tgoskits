use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Context;
use cargo_metadata::MetadataCommand;
use ostool::build::config::Cargo;
pub use ostool::build::config::LogLevel;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{context::IBuildConfig, process::ProcessExt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AxFeaturePrefixFamily {
    AxStd,
    AxFeat,
}

impl AxFeaturePrefixFamily {
    fn prefix(self) -> &'static str {
        match self {
            Self::AxStd => "axstd/",
            Self::AxFeat => "axfeat/",
        }
    }
}

#[derive(Debug, Clone, JsonSchema, Deserialize, Serialize)]
pub struct BuildConfig {
    /// Environment variables to set during the build.
    pub env: HashMap<String, String>,
    /// Target triple (e.g., "aarch64-unknown-none-softfloat", "riscv64gc-unknown-none-elf").
    pub target: String,
    /// Package name to build.
    pub package: String,
    /// Cargo features to enable.
    pub features: Vec<String>,
    /// Log level feature to automatically enable.
    pub log: LogLevel,
    /// Whether to use dynamic platform.
    pub plat_dyn: bool,
}

impl BuildConfig {
    fn resolve_features(&mut self) {
        self.resolve_features_with_manifest_path(None);
    }

    fn resolve_features_with_manifest_path(&mut self, manifest_path: Option<&Path>) {
        let prefix_family = self.resolve_ax_feature_prefix_family(manifest_path);
        let has_myplat = self.features.iter().any(|feature| {
            matches!(
                feature.as_str(),
                "myplat" | "axstd/myplat" | "axfeat/myplat"
            )
        });

        self.features.retain(|feature| {
            !matches!(
                feature.as_str(),
                "plat-dyn"
                    | "defplat"
                    | "myplat"
                    | "axstd/plat-dyn"
                    | "axstd/defplat"
                    | "axstd/myplat"
                    | "axfeat/plat-dyn"
                    | "axfeat/defplat"
                    | "axfeat/myplat"
            )
        });

        if self.plat_dyn {
            self.features
                .push(format!("{}plat-dyn", prefix_family.prefix()));
        } else if has_myplat {
            self.features
                .push(format!("{}myplat", prefix_family.prefix()));
        } else {
            self.features
                .push(format!("{}defplat", prefix_family.prefix()));
        }

        self.features.sort();
        self.features.dedup();
    }

    fn resolve_ax_feature_prefix_family(
        &self,
        manifest_path: Option<&Path>,
    ) -> AxFeaturePrefixFamily {
        match detect_ax_feature_prefix_family(&self.package, manifest_path) {
            Ok(prefix_family) => prefix_family,
            Err(err) => {
                if let Some(prefix_family) = feature_family_from_existing_features(&self.features) {
                    return prefix_family;
                }
                warn!(
                    "failed to detect direct ax dependency for package {}: {}, defaulting to \
                     axstd feature prefix",
                    self.package, err
                );
                AxFeaturePrefixFamily::AxStd
            }
        }
    }

    fn perper_env(&mut self) {
        self.env
            .insert("AX_LOG".into(), format!("{:?}", self.log).to_lowercase());
    }

    fn prepare_non_dynamic_platform(&mut self) -> anyhow::Result<()> {
        if self.plat_dyn {
            return Ok(());
        }

        let package_manifest = resolve_package_manifest_path(&self.package, None)?;
        let app_dir = package_manifest
            .parent()
            .context("package manifest path has no parent directory")?;
        let platform_package =
            resolve_platform_package(&self.package, &self.target, &self.features)?;
        let platform_config = resolve_platform_config_path(app_dir, &platform_package)?;
        let platform_name = read_platform_name(&platform_config)
            .unwrap_or_else(|| linker_platform_name(&platform_package).to_string());
        let out_config = app_dir.join(".axconfig.toml");

        generate_axconfig(
            &workspace_root_path()?,
            &self.target,
            &platform_name,
            &platform_config,
            &out_config,
        )?;

        self.env.insert(
            "AX_CONFIG_PATH".to_string(),
            out_config.display().to_string(),
        );
        self.env
            .insert("AX_PLATFORM".to_string(), platform_name.to_string());

        Ok(())
    }

    fn build_cargo_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        args.push("--config".to_string());
        args.push(if self.plat_dyn {
            format!(
                "target.{}.rustflags=[\"-Clink-arg=-Taxplat.x\"]",
                self.target
            )
        } else {
            format!(
                "target.{}.rustflags=[\"-Clink-arg=-Tlinker.x\",\"-Clink-arg=-no-pie\",\"\
                 -Clink-arg=-znostart-stop-gc\"]",
                self.target
            )
        });
        args
    }
}

impl Default for BuildConfig {
    fn default() -> Self {
        let mut env = HashMap::new();
        env.insert("AX_IP".to_string(), "10.0.2.15".to_string());
        env.insert("AX_GW".to_string(), "10.0.2.2".to_string());

        Self {
            env,
            target: "aarch64-unknown-none-softfloat".to_string(),
            package: "arceos-helloworld".to_string(),
            plat_dyn: true,
            log: LogLevel::Info,
            features: vec!["axstd".to_string()],
        }
    }
}

impl IBuildConfig for BuildConfig {
    fn to_cargo_config(mut self) -> anyhow::Result<Cargo> {
        self.perper_env();
        self.prepare_non_dynamic_platform()?;
        let args = self.build_cargo_args();
        self.resolve_features();

        Ok(Cargo {
            env: self.env,
            target: self.target,
            package: self.package,
            features: self.features,
            log: Some(self.log),
            extra_config: None,
            args,
            pre_build_cmds: vec![],
            post_build_cmds: vec![],
            to_bin: true,
        })
    }
}

fn feature_family_from_existing_features(features: &[String]) -> Option<AxFeaturePrefixFamily> {
    if features.iter().any(|feature| feature.starts_with("axstd/")) {
        return Some(AxFeaturePrefixFamily::AxStd);
    }
    if features
        .iter()
        .any(|feature| feature.starts_with("axfeat/"))
    {
        return Some(AxFeaturePrefixFamily::AxFeat);
    }
    None
}

fn detect_ax_feature_prefix_family(
    package: &str,
    manifest_path: Option<&Path>,
) -> anyhow::Result<AxFeaturePrefixFamily> {
    let mut command = MetadataCommand::new();
    command.no_deps();
    if let Some(manifest_path) = manifest_path {
        command.manifest_path(manifest_path);
    }

    let metadata = command.exec()?;
    let workspace_members: std::collections::HashSet<_> =
        metadata.workspace_members.iter().cloned().collect();
    let package_info = metadata
        .packages
        .iter()
        .find(|pkg| workspace_members.contains(&pkg.id) && pkg.name == package)
        .ok_or_else(|| anyhow::anyhow!("workspace package `{package}` not found"))?;

    let has_axstd = package_info
        .dependencies
        .iter()
        .any(|dep| dep.name == "axstd" || dep.rename.as_deref() == Some("axstd"));
    let has_axfeat = package_info
        .dependencies
        .iter()
        .any(|dep| dep.name == "axfeat" || dep.rename.as_deref() == Some("axfeat"));

    match (has_axstd, has_axfeat) {
        (true, true) | (true, false) => Ok(AxFeaturePrefixFamily::AxStd),
        (false, true) => Ok(AxFeaturePrefixFamily::AxFeat),
        (false, false) => Err(anyhow::anyhow!(
            "package `{package}` must directly depend on `axstd` or `axfeat`"
        )),
    }
}

fn resolve_package_manifest_path(
    package: &str,
    manifest_path: Option<&Path>,
) -> anyhow::Result<PathBuf> {
    let mut command = MetadataCommand::new();
    command.no_deps();
    if let Some(manifest_path) = manifest_path {
        command.manifest_path(manifest_path);
    }

    let metadata = command.exec()?;
    let workspace_members: std::collections::HashSet<_> =
        metadata.workspace_members.iter().cloned().collect();
    metadata
        .packages
        .iter()
        .find(|pkg| workspace_members.contains(&pkg.id) && pkg.name == package)
        .map(|pkg| pkg.manifest_path.clone().into_std_path_buf())
        .ok_or_else(|| anyhow::anyhow!("workspace package `{package}` not found"))
}

fn resolve_platform_package(
    package: &str,
    target: &str,
    features: &[String],
) -> anyhow::Result<String> {
    let arch = target_arch_name(target)?;
    let manifest_path = resolve_package_manifest_path(package, None)?;
    let mut command = MetadataCommand::new();
    command.no_deps().manifest_path(&manifest_path);
    let metadata = command.exec()?;
    let package_info = metadata
        .packages
        .iter()
        .find(|pkg| pkg.name == package)
        .ok_or_else(|| anyhow!("workspace package `{package}` not found"))?;

    let explicit_platform_features: Vec<_> = features
        .iter()
        .map(|feature| {
            feature
                .strip_prefix("axfeat/")
                .or_else(|| feature.strip_prefix("axstd/"))
                .unwrap_or(feature.as_str())
        })
        .filter(|feature| {
            !matches!(
                *feature,
                "axstd" | "axfeat" | "plat-dyn" | "defplat" | "myplat"
            )
        })
        .collect();

    if let Some(dep) = package_info.dependencies.iter().find(|dep| {
        dep.name.starts_with("axplat-")
            && explicit_platform_features
                .iter()
                .any(|feature| *feature == linker_platform_name(&dep.name))
    }) {
        return Ok(dep.name.clone());
    }

    if features.iter().any(|feature| {
        matches!(
            feature.as_str(),
            "myplat" | "axstd/myplat" | "axfeat/myplat"
        )
    }) && let Some(dep) = package_info
        .dependencies
        .iter()
        .find(|dep| dep.name.starts_with(&format!("axplat-{arch}")))
    {
        return Ok(dep.name.clone());
    }

    Ok(default_platform_package(arch).to_string())
}

fn target_arch_name(target: &str) -> anyhow::Result<&'static str> {
    if target.starts_with("aarch64-") {
        Ok("aarch64")
    } else if target.starts_with("x86_64-") {
        Ok("x86_64")
    } else if target.starts_with("riscv64") {
        Ok("riscv64")
    } else if target.starts_with("loongarch64-") {
        Ok("loongarch64")
    } else {
        Err(anyhow!("unsupported target triple `{target}`"))
    }
}

fn default_platform_package(arch: &str) -> &'static str {
    match arch {
        "x86_64" => "axplat-x86-pc",
        "aarch64" => "axplat-aarch64-qemu-virt",
        "riscv64" => "axplat-riscv64-qemu-virt",
        "loongarch64" => "axplat-loongarch64-qemu-virt",
        _ => unreachable!("unsupported arch"),
    }
}

fn linker_platform_name(platform_package: &str) -> &str {
    platform_package
        .strip_prefix("axplat-")
        .unwrap_or(platform_package)
}

fn resolve_platform_config_path(app_dir: &Path, platform_package: &str) -> anyhow::Result<PathBuf> {
    let output = Command::new("cargo")
        .arg("axplat")
        .arg("info")
        .arg("-C")
        .arg(app_dir)
        .arg("-c")
        .arg(platform_package)
        .exec_capture()
        .with_context(|| format!("failed to run cargo axplat info for `{platform_package}`"))?;

    let config_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if config_path.is_empty() {
        bail!(
            "cargo axplat info returned empty config path for package `{}`",
            platform_package
        );
    }

    let config_path = PathBuf::from(config_path);
    if !config_path.exists() {
        bail!(
            "platform config path does not exist: {}",
            config_path.display()
        );
    }

    Ok(config_path)
}

fn read_platform_name(platform_config: &Path) -> Option<String> {
    let contents = fs::read_to_string(platform_config).ok()?;
    let value: toml::Value = toml::from_str(&contents).ok()?;
    value
        .get("platform")
        .and_then(|value| value.as_str())
        .map(|value| value.to_string())
}

fn generate_axconfig(
    workspace_root: &Path,
    target: &str,
    platform_name: &str,
    platform_config: &Path,
    out_config: &Path,
) -> anyhow::Result<()> {
    let defconfig = resolve_defconfig_path(workspace_root)?;
    let arch = target_arch_name(target)?;

    Command::new("axconfig-gen")
        .arg(defconfig)
        .arg(platform_config)
        .arg("-w")
        .arg(format!("arch=\"{arch}\""))
        .arg("-w")
        .arg(format!("platform=\"{platform_name}\""))
        .arg("-o")
        .arg(out_config)
        .exec()
        .context("failed to run axconfig-gen")?;

    Ok(())
}

fn workspace_root_path() -> anyhow::Result<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("failed to locate workspace root from axbuild crate")?;
    Ok(root.to_path_buf())
}

fn resolve_defconfig_path(workspace_root: &Path) -> anyhow::Result<PathBuf> {
    let path = workspace_root.join("os/arceos/configs/defconfig.toml");
    if path.exists() {
        Ok(path)
    } else {
        Err(anyhow::anyhow!(
            "defconfig.toml not found at {}",
            path.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    fn base_config() -> BuildConfig {
        BuildConfig {
            plat_dyn: true,
            ..BuildConfig::default()
        }
    }

    #[test]
    fn resolves_dynamic_platform_features_and_args() {
        let mut config = base_config();
        config.resolve_features();

        assert!(config.features.contains(&"axstd/plat-dyn".to_string()));
        assert!(!config.features.contains(&"axstd/defplat".to_string()));

        let args = config.build_cargo_args();
        assert!(args.iter().any(|arg| arg.contains("-Taxplat.x")));
    }

    #[test]
    fn resolves_non_dynamic_platform_features_and_args() {
        let mut config = BuildConfig {
            plat_dyn: false,
            ..base_config()
        };
        config.resolve_features();

        assert!(config.features.contains(&"axstd/defplat".to_string()));
        assert!(!config.features.contains(&"axstd/plat-dyn".to_string()));

        let args = config.build_cargo_args();
        assert!(args.iter().any(|arg| arg.contains("-Tlinker.x")));
    }

    #[test]
    fn preserves_axstd_myplat_for_non_dynamic_platforms() {
        let mut config = BuildConfig {
            plat_dyn: false,
            features: vec!["axstd".to_string(), "axstd/myplat".to_string()],
            ..BuildConfig::default()
        };
        config.resolve_features();

        assert!(config.features.contains(&"axstd/myplat".to_string()));
        assert!(!config.features.contains(&"axstd/defplat".to_string()));
    }

    #[test]
    fn normalizes_myplat_to_axfeat_when_package_depends_on_axfeat() {
        let workspace = temp_workspace("axfeat-app", "axfeat = \"0.1.0\"\n").unwrap();
        let mut config = BuildConfig {
            package: "axfeat-app".to_string(),
            plat_dyn: false,
            features: vec!["axstd/myplat".to_string()],
            ..BuildConfig::default()
        };

        let family =
            detect_ax_feature_prefix_family("axfeat-app", Some(&workspace.join("Cargo.toml")))
                .unwrap();
        assert_eq!(family, AxFeaturePrefixFamily::AxFeat);

        config.features.retain(|feature| feature != "axstd");
        config.resolve_features_with_manifest_path(Some(&workspace.join("Cargo.toml")));

        assert!(config.features.contains(&"axfeat/myplat".to_string()));
        assert!(!config.features.contains(&"axstd/myplat".to_string()));
        assert!(!config.features.contains(&"axfeat/defplat".to_string()));
    }

    #[test]
    fn detects_axfeat_direct_dependency_via_metadata() {
        let workspace = temp_workspace("axfeat-app", "axfeat = \"0.1.0\"\n").unwrap();

        let family =
            detect_ax_feature_prefix_family("axfeat-app", Some(&workspace.join("Cargo.toml")))
                .unwrap();

        assert_eq!(family, AxFeaturePrefixFamily::AxFeat);
    }

    #[test]
    fn to_cargo_config_includes_ax_log_env() {
        let cargo = BuildConfig::default().to_cargo_config().unwrap();

        assert_eq!(cargo.env.get("AX_LOG"), Some(&"info".to_string()));
    }

    #[test]
    fn build2_toml_equivalent_config_converts_to_non_dynamic_cargo() {
        let toml = r#"
target = "aarch64-unknown-none-softfloat"
package = "arceos-helloworld"
features = ["axstd"]
log = "Info"
plat_dyn = false

[env]
AX_IP = "10.0.2.15"
AX_GW = "10.0.2.2"
"#;

        let config: BuildConfig = toml::from_str(toml).expect("config should deserialize");
        let app_dir = resolve_package_manifest_path("arceos-helloworld", None)
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let generated_config = app_dir.join(".axconfig.toml");
        let existed = generated_config.exists();

        let cargo = config.to_cargo_config().unwrap();

        assert!(cargo.features.contains(&"axstd/defplat".to_string()));
        assert!(!cargo.features.contains(&"axstd/plat-dyn".to_string()));
        assert!(cargo.args.iter().any(|arg| arg.contains("-Tlinker.x")));
        assert_eq!(
            cargo.env.get("AX_CONFIG_PATH"),
            Some(&generated_config.display().to_string())
        );
        assert_eq!(
            cargo.env.get("AX_PLATFORM"),
            Some(&"aarch64-qemu-virt".to_string())
        );

        if !existed && generated_config.exists() {
            fs::remove_file(generated_config).unwrap();
        }
    }

    fn temp_workspace(
        package_name: &str,
        dependency_block: &str,
    ) -> anyhow::Result<std::path::PathBuf> {
        let root = tempdir()?.keep();

        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\"]\nresolver = \"3\"\n\n[workspace.package]\nedition = \
             \"2024\"\n",
        )?;

        let app_dir = root.join("app");
        fs::create_dir_all(&app_dir)?;
        fs::write(
            app_dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{package_name}\"\nversion = \"0.1.0\"\nedition = \
                 \"2024\"\n\n[dependencies]\n{dependency_block}"
            ),
        )?;
        fs::create_dir_all(app_dir.join("src"))?;
        fs::write(app_dir.join("src/lib.rs"), "pub fn smoke() {}\n")?;

        Ok(root)
    }
}
