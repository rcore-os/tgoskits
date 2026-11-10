use std::path::Path;

use anyhow::bail;
use ax_config::{CommonConfig, RuntimeEnv, resolve_runtime};
use ostool::build::config::Cargo;

use super::{KtestRuntime, KtestUnitConfig};
use crate::{
    arceos, axvisor,
    context::{ResolvedAxvisorRequest, ResolvedBuildRequest, ResolvedStarryRequest},
    starry,
};

/// One parsed OS input and one environment snapshot for an execution unit.
#[derive(Debug)]
pub(super) struct KtestBuildConfig {
    config: OsConfig,
    runtime_env: RuntimeEnv,
    environment_features: Vec<String>,
}

#[derive(Debug)]
enum OsConfig {
    Arceos(arceos::config::ArceosConfig),
    Starry(starry::config::StarryConfig),
    Axvisor(axvisor::config::AxvisorConfig),
}

impl KtestBuildConfig {
    pub(super) fn load(runtime: KtestRuntime, path: &Path) -> anyhow::Result<Self> {
        let runtime_env = RuntimeEnv::capture()?;
        let mut config = match runtime {
            KtestRuntime::Arceos | KtestRuntime::Board => {
                OsConfig::Arceos(crate::build::load_build_config(path)?)
            }
            KtestRuntime::Starry => OsConfig::Starry(crate::build::load_build_config(path)?),
            KtestRuntime::Axvisor => OsConfig::Axvisor(crate::build::load_build_config(path)?),
        };
        let common = match &mut config {
            OsConfig::Arceos(config) => &mut config.common,
            OsConfig::Starry(config) => &mut config.common,
            OsConfig::Axvisor(config) => &mut config.common,
        };
        let environment_features = crate::build::makefile_features_from_env();
        crate::build::apply_makefile_features(common, &environment_features)?;
        Ok(Self {
            config,
            runtime_env,
            environment_features,
        })
    }

    fn common(&self) -> &CommonConfig {
        match &self.config {
            OsConfig::Arceos(config) => &config.common,
            OsConfig::Starry(config) => &config.common,
            OsConfig::Axvisor(config) => &config.common,
        }
    }

    pub(super) fn target(&self) -> Option<&str> {
        self.common().target.as_deref()
    }

    pub(super) fn cargo(
        &self,
        workspace: &crate::context::WorkspaceContext,
        unit: &KtestUnitConfig<'_>,
        no_default_features: bool,
    ) -> anyhow::Result<Cargo> {
        match &self.config {
            OsConfig::Arceos(config) => {
                let mut config = config.clone();
                config.common.target = Some(unit.target.to_string());
                config.package = Some(unit.package.to_string());
                config.validate()?;
                let runtime = resolve_runtime(
                    config.common.max_cpu_num,
                    config.common.runtime,
                    &self.runtime_env,
                )?;
                let request = ResolvedBuildRequest {
                    package: unit.package.to_string(),
                    arch: unit.arch.to_string(),
                    target: unit.target.to_string(),
                    smp: None,
                    debug: false,
                    build_info_path: unit.build_config.to_path_buf(),
                    config,
                    runtime,
                    runtime_env: self.runtime_env.clone(),
                    qemu_config: None,
                    uboot_config: None,
                };
                arceos::build::load_cargo_config_with_default_features(
                    &request,
                    workspace,
                    !no_default_features,
                )
            }
            OsConfig::Starry(_) => {
                let request =
                    self.starry_request(unit.package, unit.arch, unit.target, unit.build_config)?;
                starry::build::load_cargo_config(&request, workspace)
            }
            OsConfig::Axvisor(_) => {
                let request = self.axvisor_request(workspace.root(), unit)?;
                axvisor::build::load_cargo_config(&request, workspace)
            }
        }
    }

    pub(super) fn starry_request(
        &self,
        package: &str,
        arch: &str,
        target: &str,
        path: &Path,
    ) -> anyhow::Result<ResolvedStarryRequest> {
        let OsConfig::Starry(config) = &self.config else {
            bail!("Starry postprocessing requires a Starry build input");
        };
        let mut config = config.clone();
        config.common.target = Some(target.to_string());
        config.validate()?;
        let runtime = resolve_runtime(
            config.common.max_cpu_num,
            config.common.runtime,
            &self.runtime_env,
        )?;
        Ok(ResolvedStarryRequest {
            package: package.to_string(),
            arch: arch.to_string(),
            target: target.to_string(),
            smp: None,
            debug: false,
            build_info_path: path.to_path_buf(),
            config,
            runtime,
            runtime_env: self.runtime_env.clone(),
            environment_features: self.environment_features.clone(),
            qemu_config: None,
            uboot_config: None,
        })
    }

    pub(super) fn axvisor_request(
        &self,
        workspace_root: &Path,
        unit: &KtestUnitConfig<'_>,
    ) -> anyhow::Result<ResolvedAxvisorRequest> {
        let OsConfig::Axvisor(config) = &self.config else {
            bail!("Axvisor packaging requires an Axvisor build input");
        };
        let mut config = config.clone();
        config.common.target = Some(unit.target.to_string());
        config.validate()?;
        let runtime = resolve_runtime(
            config.common.max_cpu_num,
            config.common.runtime,
            &self.runtime_env,
        )?;
        Ok(ResolvedAxvisorRequest {
            package: unit.package.to_string(),
            axvisor_dir: workspace_root.join("os/axvisor"),
            arch: unit.arch.to_string(),
            target: unit.target.to_string(),
            smp: None,
            debug: false,
            build_info_path: unit.build_config.to_path_buf(),
            config,
            runtime,
            runtime_env: self.runtime_env.clone(),
            environment_features: self.environment_features.clone(),
            qemu_config: None,
            uboot_config: None,
            vmconfigs: Vec::new(),
        })
    }
}
