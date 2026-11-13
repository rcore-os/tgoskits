use std::path::Path;

use anyhow::bail;
use ax_config::{CommonConfig, RuntimeEnv, resolve_runtime};
use ostool::build::config::Cargo;

use super::{KtestRuntime, KtestUnitConfig};
use crate::{
    arceos, axvisor,
    build::{BuildConfiguration, ConfigInput, ConfigSource, load_config_input},
    context::{
        RequestEnvironment, ResolvedAxvisorRequest, ResolvedBuildRequest, ResolvedStarryRequest,
    },
    starry,
};

/// One parsed OS input and one environment snapshot for an execution unit.
#[derive(Debug)]
pub(super) struct KtestBuildConfig {
    config: OsConfig,
    source: ConfigSource,
    target_constraint: Option<String>,
    runtime_env: RuntimeEnv,
    environment_features: Vec<String>,
}

pub(super) struct KtestBuildSelection<'a> {
    pub runtime: KtestRuntime,
    pub config: Option<&'a Path>,
    pub preset: Option<&'a str>,
    pub default_qemu_target: Option<&'a str>,
}

#[derive(Debug)]
enum OsConfig {
    Arceos(arceos::config::ArceosConfig),
    Starry(starry::config::StarryConfig),
    Axvisor(axvisor::config::AxvisorConfig),
}

impl KtestBuildConfig {
    pub(super) fn load(
        workspace_root: &Path,
        selection: KtestBuildSelection<'_>,
    ) -> anyhow::Result<Self> {
        let environment = RequestEnvironment::capture()?;
        let (mut config, source, target_constraint) = match selection.runtime {
            KtestRuntime::Arceos | KtestRuntime::Board => {
                let input = select_input(workspace_root, &selection)?;
                (
                    OsConfig::Arceos(input.config),
                    input.source,
                    input.target_constraint,
                )
            }
            KtestRuntime::Starry => {
                let input = select_input(workspace_root, &selection)?;
                (
                    OsConfig::Starry(input.config),
                    input.source,
                    input.target_constraint,
                )
            }
            KtestRuntime::Axvisor => {
                let input = select_input(workspace_root, &selection)?;
                (
                    OsConfig::Axvisor(input.config),
                    input.source,
                    input.target_constraint,
                )
            }
        };
        let common = match &mut config {
            OsConfig::Arceos(config) => &mut config.common,
            OsConfig::Starry(config) => &mut config.common,
            OsConfig::Axvisor(config) => &mut config.common,
        };
        let environment_features = environment.features;
        crate::build::apply_makefile_features(common, &environment_features)?;
        Ok(Self {
            config,
            source,
            target_constraint,
            runtime_env: environment.runtime,
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

    pub(super) fn config_path(&self) -> Option<&Path> {
        self.source.path()
    }

    fn check_target(&self, arch: &str, target: &str) -> anyhow::Result<()> {
        if let Some(expected) = &self.target_constraint {
            anyhow::ensure!(
                expected == target,
                "preset target `{expected}` conflicts with ktest target `{target}`"
            );
            anyhow::ensure!(
                crate::context::arch_for_target_checked(expected)? == arch,
                "preset target `{expected}` conflicts with ktest architecture `{arch}`"
            );
        }
        Ok(())
    }

    pub(super) fn cargo(
        &self,
        workspace: &crate::context::WorkspaceContext,
        unit: &KtestUnitConfig<'_>,
        no_default_features: bool,
    ) -> anyhow::Result<Cargo> {
        self.check_target(unit.arch, unit.target)?;
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
                    source: self.source.clone(),
                    config,
                    runtime,
                    runtime_env: self.runtime_env.clone(),
                    environment_features: self.environment_features.clone(),
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
                let request = self.starry_request(unit.package, unit.arch, unit.target)?;
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
    ) -> anyhow::Result<ResolvedStarryRequest> {
        self.check_target(arch, target)?;
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
            source: self.source.clone(),
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
        self.check_target(unit.arch, unit.target)?;
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
            source: self.source.clone(),
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

fn select_input<T: BuildConfiguration>(
    workspace_root: &Path,
    selection: &KtestBuildSelection<'_>,
) -> anyhow::Result<ConfigInput<T>> {
    let mut input = load_config_input(workspace_root, selection.config, selection.preset)?;
    if let Some(target) = selection.default_qemu_target {
        input.select_default_qemu(target)?;
    }
    Ok(input)
}
