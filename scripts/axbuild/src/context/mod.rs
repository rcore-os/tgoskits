use std::{
    fmt::Debug,
    path::{Path, PathBuf},
};

use ostool::{
    Tool, ToolConfig,
    build::{CargoRunnerKind, config::Cargo},
};
use schemars::JsonSchema;
use serde::{Serialize, de::DeserializeOwned};

#[derive(Debug, Clone)]
pub struct QemuConfig {
    pub build_config: Option<PathBuf>,
    pub qemu_config: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildConfigLookupKey {
    pub os: String,
    pub package: Option<String>,
    pub target: Option<String>,
}

impl BuildConfigLookupKey {
    pub fn new(os: impl Into<String>, package: Option<String>, target: Option<String>) -> Self {
        Self {
            os: os.into(),
            package,
            target,
        }
    }

    pub fn file_name(&self) -> String {
        format!(
            ".{}_{}_{}.toml",
            self.os,
            self.package.as_deref().unwrap_or_default(),
            self.target.as_deref().unwrap_or_default()
        )
    }

    fn resolve_path(&self, root: &Path) -> PathBuf {
        root.join(self.file_name())
    }
}

pub trait IBuildConfig: Clone + Debug + JsonSchema + DeserializeOwned + Serialize {
    fn to_cargo_config(self) -> anyhow::Result<Cargo>;
}

pub struct AppContext {
    tool: Tool,
    build_config_path: Option<PathBuf>,
    qemu_config_path: Option<PathBuf>,
    root: PathBuf,
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
            qemu_config_path: None,
            root: workspace_root,
        })
    }

    pub async fn build(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    pub async fn qemu<T: IBuildConfig>(
        &mut self,
        qemu_config: QemuConfig,
        def_config: T,
        lookup_key: BuildConfigLookupKey,
    ) -> anyhow::Result<()> {
        let config = self
            .perper_qemu_config::<T>(qemu_config, def_config, lookup_key)
            .await?;

        let kind = CargoRunnerKind::Qemu {
            qemu_config: self.qemu_config_path.clone(),
            debug: false,
            dtb_dump: false,
        };

        self.tool.cargo_run(&config, &kind).await?;

        Ok(())
    }

    pub async fn uboot<T: IBuildConfig>(
        &mut self,
        build_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
        def_config: T,
        lookup_key: BuildConfigLookupKey,
    ) -> anyhow::Result<()> {
        let cargo = self
            .perper_build_config(build_config, def_config, lookup_key)
            .await?;

        let kind = CargoRunnerKind::Uboot { uboot_config };

        self.tool.cargo_run(&cargo, &kind).await?;

        Ok(())
    }

    async fn perper_build_config<T: IBuildConfig>(
        &mut self,
        build_config: Option<PathBuf>,
        def_config: T,
        lookup_key: BuildConfigLookupKey,
    ) -> anyhow::Result<Cargo> {
        let build_config_path = resolve_build_config_path(&self.root, build_config, &lookup_key);

        println!("Using build config: {}", build_config_path.display());

        if build_config_path.exists() {
            info!("Found build config at {}", build_config_path.display());
        } else {
            info!(
                "Build config not found at {}, using default config",
                build_config_path.display()
            );
            // Write default config to the path
            let default_build = def_config;
            let toml_str = toml::to_string_pretty(&default_build)?;
            std::fs::write(&build_config_path, toml_str)?;
            info!(
                "Default build config written to {}",
                build_config_path.display()
            );
        }

        let config = toml::from_str::<T>(&std::fs::read_to_string(&build_config_path)?)?;
        let cargo = config.to_cargo_config()?;

        self.build_config_path = Some(build_config_path);

        Ok(cargo)
    }

    async fn perper_qemu_config<T: IBuildConfig>(
        &mut self,
        config: QemuConfig,
        def_config: T,
        lookup_key: BuildConfigLookupKey,
    ) -> anyhow::Result<Cargo> {
        self.qemu_config_path = config.qemu_config;
        let cargo = self
            .perper_build_config::<T>(config.build_config, def_config, lookup_key)
            .await?;

        Ok(cargo)
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

fn resolve_build_config_path(
    root: &Path,
    build_config: Option<PathBuf>,
    lookup_key: &BuildConfigLookupKey,
) -> PathBuf {
    build_config.unwrap_or_else(|| lookup_key.resolve_path(root))
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, fs};

    use tempfile::tempdir;

    use super::*;

    #[derive(Debug, Clone, Serialize, serde::Deserialize, schemars::JsonSchema)]
    struct TestBuildConfig {
        value: String,
    }

    impl IBuildConfig for TestBuildConfig {
        fn to_cargo_config(self) -> anyhow::Result<Cargo> {
            Ok(Cargo {
                env: HashMap::new(),
                target: "dummy-target".to_string(),
                package: self.value,
                features: vec![],
                log: None,
                extra_config: None,
                args: vec![],
                pre_build_cmds: vec![],
                post_build_cmds: vec![],
                to_bin: true,
            })
        }
    }

    #[test]
    fn build_config_lookup_key_formats_full_name() {
        let key = BuildConfigLookupKey::new(
            "arceos",
            Some("arceos-helloworld".to_string()),
            Some("aarch64-unknown-none-softfloat".to_string()),
        );

        assert_eq!(
            key.file_name(),
            ".arceos_arceos-helloworld_aarch64-unknown-none-softfloat.toml"
        );
    }

    #[test]
    fn build_config_lookup_key_formats_missing_package() {
        let key = BuildConfigLookupKey::new(
            "arceos",
            None,
            Some("aarch64-unknown-none-softfloat".to_string()),
        );

        assert_eq!(
            key.file_name(),
            ".arceos__aarch64-unknown-none-softfloat.toml"
        );
    }

    #[test]
    fn build_config_lookup_key_formats_missing_target() {
        let key = BuildConfigLookupKey::new("arceos", Some("arceos-helloworld".to_string()), None);

        assert_eq!(key.file_name(), ".arceos_arceos-helloworld_.toml");
    }

    #[test]
    fn build_config_lookup_key_formats_missing_package_and_target() {
        let key = BuildConfigLookupKey::new("arceos", None, None);

        assert_eq!(key.file_name(), ".arceos__.toml");
    }

    #[test]
    fn resolve_build_config_path_prefers_explicit_path() {
        let root = PathBuf::from("/tmp/workspace");
        let explicit = PathBuf::from("/tmp/custom.toml");
        let key = BuildConfigLookupKey::new("arceos", None, None);

        assert_eq!(
            resolve_build_config_path(&root, Some(explicit.clone()), &key),
            explicit
        );
    }

    #[test]
    fn resolve_build_config_path_uses_new_lookup_name() {
        let root = PathBuf::from("/tmp/workspace");
        let key = BuildConfigLookupKey::new(
            "arceos",
            Some("arceos-helloworld".to_string()),
            Some("aarch64-unknown-none-softfloat".to_string()),
        );

        assert_eq!(
            resolve_build_config_path(&root, None, &key),
            root.join(".arceos_arceos-helloworld_aarch64-unknown-none-softfloat.toml")
        );
    }

    #[tokio::test]
    async fn perper_build_config_reads_existing_new_format_file() {
        let root = tempdir().unwrap();
        let key = BuildConfigLookupKey::new(
            "arceos",
            Some("pkg".to_string()),
            Some("target".to_string()),
        );
        let path = root.path().join(key.file_name());
        fs::write(&path, "value = \"from-file\"\n").unwrap();

        let mut app = AppContext {
            tool: Tool::new(ToolConfig::default()).unwrap(),
            build_config_path: None,
            qemu_config_path: None,
            root: root.path().to_path_buf(),
        };

        let cargo = app
            .perper_build_config(
                None,
                TestBuildConfig {
                    value: "default".into(),
                },
                key,
            )
            .await
            .unwrap();

        assert_eq!(cargo.package, "from-file");
        assert_eq!(app.build_config_path, Some(path));
    }

    #[tokio::test]
    async fn perper_build_config_creates_missing_new_format_file() {
        let root = tempdir().unwrap();
        let key = BuildConfigLookupKey::new("arceos", None, Some("target".to_string()));
        let path = root.path().join(key.file_name());

        let mut app = AppContext {
            tool: Tool::new(ToolConfig::default()).unwrap(),
            build_config_path: None,
            qemu_config_path: None,
            root: root.path().to_path_buf(),
        };

        let cargo = app
            .perper_build_config(
                None,
                TestBuildConfig {
                    value: "default".into(),
                },
                key,
            )
            .await
            .unwrap();

        assert_eq!(cargo.package, "default");
        assert!(path.exists());
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains("value = \"default\"")
        );
    }

    #[tokio::test]
    async fn perper_build_config_does_not_fall_back_to_legacy_name() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join(".build-target.toml"),
            "value = \"legacy\"\n",
        )
        .unwrap();
        let key = BuildConfigLookupKey::new("arceos", None, Some("target".to_string()));
        let new_path = root.path().join(key.file_name());

        let mut app = AppContext {
            tool: Tool::new(ToolConfig::default()).unwrap(),
            build_config_path: None,
            qemu_config_path: None,
            root: root.path().to_path_buf(),
        };

        let cargo = app
            .perper_build_config(
                None,
                TestBuildConfig {
                    value: "default".into(),
                },
                key,
            )
            .await
            .unwrap();

        assert_eq!(cargo.package, "default");
        assert!(new_path.exists());
        assert_eq!(
            fs::read_to_string(root.path().join(".build-target.toml")).unwrap(),
            "value = \"legacy\"\n"
        );
    }
}
