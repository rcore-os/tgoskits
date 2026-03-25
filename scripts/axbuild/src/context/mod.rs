use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};
use ostool::{
    Tool, ToolConfig,
    build::{CargoRunnerKind, config::Cargo},
};
use serde::{Deserialize, Serialize};

pub const ARCEOS_SNAPSHOT_FILE: &str = ".arceos.toml";
pub const DEFAULT_ARCEOS_TARGET: &str = "aarch64-unknown-none-softfloat";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildCliArgs {
    pub config: Option<PathBuf>,
    pub package: Option<String>,
    pub target: Option<String>,
    pub no_dyn: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArceosQemuSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qemu_config: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArceosUbootSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uboot_config: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArceosCommandSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub no_dyn: bool,
    #[serde(default, skip_serializing_if = "ArceosQemuSnapshot::is_empty")]
    pub qemu: ArceosQemuSnapshot,
    #[serde(default, skip_serializing_if = "ArceosUbootSnapshot::is_empty")]
    pub uboot: ArceosUbootSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBuildRequest {
    pub package: String,
    pub target: String,
    pub no_dyn: bool,
    pub build_info_path: PathBuf,
    pub qemu_config: Option<PathBuf>,
    pub uboot_config: Option<PathBuf>,
}

pub struct AppContext {
    tool: Tool,
    build_config_path: Option<PathBuf>,
    root: PathBuf,
}

impl ArceosQemuSnapshot {
    fn is_empty(&self) -> bool {
        self.qemu_config.is_none()
    }
}

impl ArceosUbootSnapshot {
    fn is_empty(&self) -> bool {
        self.uboot_config.is_none()
    }
}

impl ArceosCommandSnapshot {
    pub fn path_in(root: &Path) -> PathBuf {
        root.join(ARCEOS_SNAPSHOT_FILE)
    }

    pub fn load(root: &Path) -> anyhow::Result<Self> {
        let path = Self::path_in(root);
        if !path.exists() {
            return Ok(Self::default());
        }

        toml::from_str(&std::fs::read_to_string(&path)?)
            .with_context(|| format!("failed to parse snapshot {}", path.display()))
    }

    pub fn store(&self, root: &Path) -> anyhow::Result<PathBuf> {
        let path = Self::path_in(root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, toml::to_string_pretty(self)?)?;
        Ok(path)
    }
}

impl AppContext {
    pub fn new() -> anyhow::Result<Self> {
        let workspace_root = find_workspace_root();
        crate::logging::init_logging(&workspace_root)?;

        info!("Workspace root: {}", workspace_root.display());

        let tool = Tool::new(ToolConfig::default()).unwrap();
        Ok(Self {
            tool,
            build_config_path: None,
            root: workspace_root,
        })
    }

    pub fn workspace_root(&self) -> &Path {
        &self.root
    }

    pub fn prepare_arceos_request(
        &self,
        cli: BuildCliArgs,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
    ) -> anyhow::Result<(ResolvedBuildRequest, ArceosCommandSnapshot)> {
        let snapshot = ArceosCommandSnapshot::load(&self.root)?;

        let package = cli
            .package
            .clone()
            .or_else(|| snapshot.package.clone())
            .ok_or_else(|| {
                anyhow!(
                    "missing ArceOS package; pass `--package` or set `package` in {}",
                    ARCEOS_SNAPSHOT_FILE
                )
            })?;
        let target = cli
            .target
            .clone()
            .or_else(|| snapshot.target.clone())
            .unwrap_or_else(|| DEFAULT_ARCEOS_TARGET.to_string());
        let no_dyn = cli.no_dyn.unwrap_or(snapshot.no_dyn);

        let resolved_qemu_config = qemu_config
            .clone()
            .or_else(|| resolve_snapshot_path(&self.root, snapshot.qemu.qemu_config.as_ref()));
        let resolved_uboot_config = uboot_config
            .clone()
            .or_else(|| resolve_snapshot_path(&self.root, snapshot.uboot.uboot_config.as_ref()));
        let build_info_path =
            crate::arceos::build::resolve_build_info_path(&package, &target, cli.config.clone())?;

        let request = ResolvedBuildRequest {
            package: package.clone(),
            target: target.clone(),
            no_dyn,
            build_info_path,
            qemu_config: resolved_qemu_config.clone(),
            uboot_config: resolved_uboot_config.clone(),
        };

        let snapshot = ArceosCommandSnapshot {
            package: Some(package),
            target: Some(target),
            no_dyn,
            qemu: ArceosQemuSnapshot {
                qemu_config: resolved_qemu_config
                    .as_ref()
                    .map(|path| snapshot_path_value(&self.root, path)),
            },
            uboot: ArceosUbootSnapshot {
                uboot_config: resolved_uboot_config
                    .as_ref()
                    .map(|path| snapshot_path_value(&self.root, path)),
            },
        };

        Ok((request, snapshot))
    }

    pub fn store_arceos_snapshot(
        &self,
        snapshot: &ArceosCommandSnapshot,
    ) -> anyhow::Result<PathBuf> {
        snapshot.store(&self.root)
    }

    pub async fn build(&mut self, cargo: Cargo, build_config_path: PathBuf) -> anyhow::Result<()> {
        self.set_build_config_path(build_config_path);
        self.tool.cargo_build(&cargo).await
    }

    pub async fn qemu(
        &mut self,
        cargo: Cargo,
        build_config_path: PathBuf,
        qemu_config: Option<PathBuf>,
    ) -> anyhow::Result<()> {
        self.set_build_config_path(build_config_path);
        self.tool
            .cargo_run(
                &cargo,
                &CargoRunnerKind::Qemu {
                    qemu_config,
                    debug: false,
                    dtb_dump: false,
                },
            )
            .await
    }

    pub async fn uboot(
        &mut self,
        cargo: Cargo,
        build_config_path: PathBuf,
        uboot_config: Option<PathBuf>,
    ) -> anyhow::Result<()> {
        self.set_build_config_path(build_config_path);
        self.tool
            .cargo_run(&cargo, &CargoRunnerKind::Uboot { uboot_config })
            .await
    }

    fn set_build_config_path(&mut self, path: PathBuf) {
        self.build_config_path = Some(path.clone());
        self.tool.ctx_mut().build_config_path = Some(path);
    }
}

impl Default for AppContext {
    fn default() -> Self {
        Self::new().expect("failed to initialize AppContext")
    }
}

fn find_workspace_root() -> PathBuf {
    let cargo = cargo_metadata::MetadataCommand::new()
        .no_deps()
        .exec()
        .expect("Failed to get cargo metadata");

    cargo.workspace_root.canonicalize().unwrap()
}

fn resolve_snapshot_path(root: &Path, path: Option<&PathBuf>) -> Option<PathBuf> {
    path.map(|path| {
        if path.is_relative() {
            root.join(path)
        } else {
            path.clone()
        }
    })
}

fn snapshot_path_value(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.strip_prefix(root)
            .map(PathBuf::from)
            .unwrap_or_else(|_| path.to_path_buf())
    } else {
        path.to_path_buf()
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn snapshot_load_returns_default_when_missing() {
        let root = tempdir().unwrap();
        let snapshot = ArceosCommandSnapshot::load(root.path()).unwrap();
        assert_eq!(snapshot, ArceosCommandSnapshot::default());
    }

    #[test]
    fn snapshot_store_round_trips() {
        let root = tempdir().unwrap();
        let snapshot = ArceosCommandSnapshot {
            package: Some("arceos-helloworld".into()),
            target: Some("target".into()),
            no_dyn: true,
            qemu: ArceosQemuSnapshot {
                qemu_config: Some(PathBuf::from("configs/qemu.toml")),
            },
            uboot: ArceosUbootSnapshot {
                uboot_config: Some(PathBuf::from("configs/uboot.toml")),
            },
        };

        let path = snapshot.store(root.path()).unwrap();
        let loaded = ArceosCommandSnapshot::load(root.path()).unwrap();

        assert_eq!(path, root.path().join(ARCEOS_SNAPSHOT_FILE));
        assert_eq!(loaded, snapshot);
    }

    #[test]
    fn prepare_request_prefers_cli_over_snapshot() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join(ARCEOS_SNAPSHOT_FILE),
            r#"
package = "from-snapshot"
target = "snapshot-target"
no_dyn = false

[qemu]
qemu_config = "configs/snapshot-qemu.toml"

[uboot]
uboot_config = "configs/snapshot-uboot.toml"
"#,
        )
        .unwrap();

        let app = AppContext {
            tool: Tool::new(ToolConfig::default()).unwrap(),
            build_config_path: None,
            root: root.path().to_path_buf(),
        };

        let (request, snapshot) = app
            .prepare_arceos_request(
                BuildCliArgs {
                    config: Some(PathBuf::from("/tmp/custom-build.toml")),
                    package: Some("from-cli".into()),
                    target: Some("cli-target".into()),
                    no_dyn: Some(true),
                },
                Some(PathBuf::from("/tmp/qemu.toml")),
                None,
            )
            .unwrap();

        assert_eq!(request.package, "from-cli");
        assert_eq!(request.target, "cli-target");
        assert!(request.no_dyn);
        assert_eq!(request.build_info_path, PathBuf::from("/tmp/custom-build.toml"));
        assert_eq!(request.qemu_config, Some(PathBuf::from("/tmp/qemu.toml")));
        assert_eq!(
            request.uboot_config,
            Some(root.path().join("configs/snapshot-uboot.toml"))
        );
        assert_eq!(snapshot.package.as_deref(), Some("from-cli"));
        assert_eq!(snapshot.target.as_deref(), Some("cli-target"));
        assert_eq!(snapshot.qemu.qemu_config, Some(PathBuf::from("/tmp/qemu.toml")));
    }

    #[test]
    fn prepare_request_uses_snapshot_and_default_target() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join(ARCEOS_SNAPSHOT_FILE),
            r#"
package = "arceos-helloworld"

[qemu]
qemu_config = "configs/qemu.toml"
"#,
        )
        .unwrap();

        let app = AppContext {
            tool: Tool::new(ToolConfig::default()).unwrap(),
            build_config_path: None,
            root: root.path().to_path_buf(),
        };

        let (request, snapshot) = app
            .prepare_arceos_request(BuildCliArgs::default(), None, None)
            .unwrap();

        assert_eq!(request.package, "arceos-helloworld");
        assert_eq!(request.target, DEFAULT_ARCEOS_TARGET);
        assert!(!request.no_dyn);
        assert_eq!(
            request.qemu_config,
            Some(root.path().join("configs/qemu.toml"))
        );
        assert_eq!(snapshot.target.as_deref(), Some(DEFAULT_ARCEOS_TARGET));
    }

    #[test]
    fn prepare_request_requires_package() {
        let root = tempdir().unwrap();
        let app = AppContext {
            tool: Tool::new(ToolConfig::default()).unwrap(),
            build_config_path: None,
            root: root.path().to_path_buf(),
        };

        let err = app
            .prepare_arceos_request(BuildCliArgs::default(), None, None)
            .unwrap_err();

        assert!(err.to_string().contains("missing ArceOS package"));
    }
}
