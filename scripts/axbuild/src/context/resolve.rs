use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};
use ax_config::{RuntimeEnv, resolve_runtime};

use super::{
    AppContext, ArceosCommandSnapshot, ArceosQemuSnapshot, ArceosUbootSnapshot, AxvisorCliArgs,
    AxvisorCommandSnapshot, AxvisorQemuSnapshot, AxvisorUbootSnapshot, BuildCliArgs,
    RequestEnvironment, ResolvedAxvisorRequest, ResolvedBuildRequest, ResolvedStarryRequest,
    STARRY_PACKAGE, StarryCliArgs, StarryCommandSnapshot, StarryQemuSnapshot, StarryUbootSnapshot,
    resolve_arceos_arch_and_target, resolve_axvisor_arch_and_target,
    resolve_starry_arch_and_target,
};
use crate::{
    arceos::{cbuild::AX_LIBC_PACKAGE, config::ArceosConfig},
    axvisor::config::AxvisorConfig,
    build::{ConfigInput, load_config_input},
    starry::config::StarryConfig,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ResolvedCommandPaths {
    qemu_config: Option<PathBuf>,
    uboot_config: Option<PathBuf>,
}

#[derive(Clone, Copy)]
enum ArceosDefault {
    RequireApplication,
    QemuHello,
}

impl AppContext {
    pub(crate) fn prepare_arceos_request(
        &self,
        cli: BuildCliArgs,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
    ) -> anyhow::Result<(ResolvedBuildRequest, ArceosCommandSnapshot)> {
        self.resolve_arceos_request(
            cli,
            None,
            qemu_config,
            uboot_config,
            ArceosDefault::RequireApplication,
            |_, _| Ok(()),
        )
    }

    pub(crate) fn prepare_arceos_qemu_request(
        &self,
        cli: BuildCliArgs,
        qemu_config: Option<PathBuf>,
    ) -> anyhow::Result<(ResolvedBuildRequest, ArceosCommandSnapshot)> {
        self.resolve_arceos_request(
            cli,
            None,
            qemu_config,
            None,
            ArceosDefault::QemuHello,
            |_, _| Ok(()),
        )
    }

    pub(crate) fn prepare_arceos_request_with_input_check(
        &self,
        cli: BuildCliArgs,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
        input_check: impl FnOnce(&ArceosConfig, crate::build::FileFields) -> anyhow::Result<()>,
    ) -> anyhow::Result<(ResolvedBuildRequest, ArceosCommandSnapshot)> {
        self.resolve_arceos_request(
            cli,
            None,
            qemu_config,
            uboot_config,
            ArceosDefault::RequireApplication,
            input_check,
        )
    }

    pub(crate) fn prepare_arceos_request_from_input(
        &self,
        cli: BuildCliArgs,
        input: ConfigInput<ArceosConfig>,
        environment: RequestEnvironment,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
    ) -> anyhow::Result<(ResolvedBuildRequest, ArceosCommandSnapshot)> {
        self.resolve_arceos_request(
            cli,
            Some((input, environment)),
            qemu_config,
            uboot_config,
            ArceosDefault::RequireApplication,
            |_, _| Ok(()),
        )
    }

    fn resolve_arceos_request(
        &self,
        cli: BuildCliArgs,
        provided: Option<(ConfigInput<ArceosConfig>, RequestEnvironment)>,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
        default: ArceosDefault,
        input_check: impl FnOnce(&ArceosConfig, crate::build::FileFields) -> anyhow::Result<()>,
    ) -> anyhow::Result<(ResolvedBuildRequest, ArceosCommandSnapshot)> {
        let (provided, environment) = match provided {
            Some((input, environment)) => (Some(input), environment),
            None => (None, RequestEnvironment::capture()?),
        };
        let runtime_env = environment.runtime;
        let environment_features = environment.features;
        let saved = ArceosCommandSnapshot::load(self.workspace_root())?;
        let inherit = provided.is_none()
            && cli.preset.is_none()
            && cli.config.is_none()
            && cli.package.is_none()
            && cli.arch.is_none()
            && cli.target.is_none();
        let path = cli
            .config
            .as_deref()
            .or_else(|| inherit.then_some(saved.config.as_deref()).flatten());
        let preset = cli
            .preset
            .as_deref()
            .or_else(|| inherit.then_some(saved.preset.as_deref()).flatten());
        let mut input: ConfigInput<ArceosConfig> = match provided {
            Some(input) => {
                input.check_preset(cli.preset.as_deref())?;
                input
            }
            None => self.load_request_input(path, preset)?,
        };
        input_check(&input.config, input.file_fields)?;
        input.check_selectors(cli.target.as_deref(), cli.arch.as_deref())?;
        let (arch, target) = resolve_arceos_arch_and_target(
            cli.arch.clone().or_else(|| {
                (inherit && cli.target.is_none() && input.config.common.target.is_none())
                    .then(|| saved.arch.clone())
                    .flatten()
            }),
            cli.target
                .clone()
                .or(input.config.common.target.clone())
                .or_else(|| {
                    (inherit && cli.arch.is_none())
                        .then(|| saved.target.clone())
                        .flatten()
                }),
        )?;
        let package = cli
            .package
            .clone()
            .or(input.config.package.clone())
            .or_else(|| inherit.then(|| saved.package.clone()).flatten());
        if package.is_none()
            && input.config.app_c.is_none()
            && matches!(default, ArceosDefault::QemuHello)
        {
            input.select_default_qemu(&target)?;
        }
        let package = package
            .or(input.config.package.clone())
            .or_else(|| {
                input
                    .config
                    .app_c
                    .as_ref()
                    .map(|_| AX_LIBC_PACKAGE.to_owned())
            })
            .ok_or_else(|| {
                anyhow!(
                    "missing ArceOS package; pass `--package`, select `--preset`, or set \
                     `package` in a custom TOML"
                )
            })?;
        let smp = cli.smp.or_else(|| inherit.then_some(saved.smp).flatten());
        let paths = self.resolve_runtime_paths(
            qemu_config,
            inherit.then_some(saved.qemu.qemu_config.as_ref()).flatten(),
            uboot_config,
            inherit
                .then_some(saved.uboot.uboot_config.as_ref())
                .flatten(),
        );
        let mut config = input.config;
        config.package = Some(package.clone());
        config.common.target = Some(target.clone());
        crate::build::apply_makefile_features(&mut config.common, &environment_features)?;
        if let Some(smp) = smp {
            config.common.max_cpu_num = Some(smp);
        }
        config.validate()?;
        if let Some(app_c) = &config.app_c {
            let base = if input.file_fields.app_c {
                input
                    .source
                    .path()
                    .and_then(Path::parent)
                    .context("app-c file source has no parent")?
            } else {
                self.workspace_root()
            };
            let app_c = std::path::absolute(base.join(app_c))?;
            config.app_c = Some(crate::arceos::build::resolve_app_c_dir(&app_c)?);
        }
        crate::build::validate_features(&config.common)?;
        let runtime = resolve_runtime(
            config.common.max_cpu_num,
            config.common.runtime,
            &runtime_env,
        )?;
        let snapshot = ArceosCommandSnapshot {
            preset: input.selection_preset,
            config: input
                .source
                .path()
                .map(|path| snapshot_path_value(self.workspace_root(), path)),
            package: Some(package.clone()),
            arch: Some(arch.clone()),
            target: Some(target.clone()),
            smp,
            qemu: ArceosQemuSnapshot {
                qemu_config: paths
                    .qemu_config
                    .as_deref()
                    .map(|path| snapshot_path_value(self.workspace_root(), path)),
            },
            uboot: ArceosUbootSnapshot {
                uboot_config: paths
                    .uboot_config
                    .as_deref()
                    .map(|path| snapshot_path_value(self.workspace_root(), path)),
            },
        };
        let request = ResolvedBuildRequest {
            package,
            arch,
            target,
            smp,
            debug: cli.debug,
            source: input.source,
            config,
            runtime,
            runtime_env,
            environment_features,
            qemu_config: paths.qemu_config,
            uboot_config: paths.uboot_config,
        };
        Ok((request, snapshot))
    }

    pub(crate) fn store_arceos_snapshot(
        &self,
        snapshot: &ArceosCommandSnapshot,
    ) -> anyhow::Result<PathBuf> {
        snapshot.store(self.workspace_root())
    }

    pub(crate) fn prepare_starry_request(
        &self,
        cli: StarryCliArgs,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
    ) -> anyhow::Result<(ResolvedStarryRequest, StarryCommandSnapshot)> {
        self.resolve_starry_request(cli, None, qemu_config, uboot_config)
    }

    pub(crate) fn prepare_starry_request_from_input(
        &self,
        cli: StarryCliArgs,
        input: ConfigInput<StarryConfig>,
        environment: RequestEnvironment,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
    ) -> anyhow::Result<(ResolvedStarryRequest, StarryCommandSnapshot)> {
        self.resolve_starry_request(cli, Some((input, environment)), qemu_config, uboot_config)
    }

    fn resolve_starry_request(
        &self,
        cli: StarryCliArgs,
        provided: Option<(ConfigInput<StarryConfig>, RequestEnvironment)>,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
    ) -> anyhow::Result<(ResolvedStarryRequest, StarryCommandSnapshot)> {
        let (provided, environment) = match provided {
            Some((input, environment)) => (Some(input), environment),
            None => (None, RequestEnvironment::capture()?),
        };
        let runtime_env = environment.runtime;
        let environment_features = environment.features;
        let saved = StarryCommandSnapshot::load(self.workspace_root())?;
        let inherit = provided.is_none()
            && cli.preset.is_none()
            && cli.config.is_none()
            && cli.arch.is_none()
            && cli.target.is_none();
        let path = cli
            .config
            .as_deref()
            .or_else(|| inherit.then_some(saved.config.as_deref()).flatten());
        let preset = cli
            .preset
            .as_deref()
            .or_else(|| inherit.then_some(saved.preset.as_deref()).flatten());
        let mut input: ConfigInput<StarryConfig> = match provided {
            Some(input) => {
                input.check_preset(cli.preset.as_deref())?;
                input
            }
            None => self.load_request_input(path, preset)?,
        };
        input.check_selectors(cli.target.as_deref(), cli.arch.as_deref())?;
        let (arch, target) = resolve_starry_arch_and_target(
            cli.arch.clone().or_else(|| {
                (inherit && cli.target.is_none() && input.config.common.target.is_none())
                    .then(|| saved.arch.clone())
                    .flatten()
            }),
            cli.target
                .clone()
                .or(input.config.common.target.clone())
                .or_else(|| {
                    (inherit && cli.arch.is_none())
                        .then(|| saved.target.clone())
                        .flatten()
                }),
        )?;
        input.select_default_qemu(&target)?;
        let smp = cli.smp.or_else(|| inherit.then_some(saved.smp).flatten());
        let paths = self.resolve_runtime_paths(
            qemu_config,
            inherit.then_some(saved.qemu.qemu_config.as_ref()).flatten(),
            uboot_config,
            inherit
                .then_some(saved.uboot.uboot_config.as_ref())
                .flatten(),
        );
        let mut config = input.config;
        config.common.target = Some(target.clone());
        crate::build::apply_makefile_features(&mut config.common, &environment_features)?;
        if let Some(smp) = smp {
            config.common.max_cpu_num = Some(smp);
        }
        config.validate()?;
        crate::build::validate_features(&config.common)?;
        config.validate_resources(self.workspace_root())?;
        let runtime = resolve_runtime(
            config.common.max_cpu_num,
            config.common.runtime,
            &runtime_env,
        )?;
        let snapshot = StarryCommandSnapshot {
            preset: input.selection_preset,
            config: input
                .source
                .path()
                .map(|path| snapshot_path_value(self.workspace_root(), path)),
            arch: Some(arch.clone()),
            target: Some(target.clone()),
            smp,
            qemu: StarryQemuSnapshot {
                qemu_config: paths
                    .qemu_config
                    .as_deref()
                    .map(|path| snapshot_path_value(self.workspace_root(), path)),
            },
            uboot: StarryUbootSnapshot {
                uboot_config: paths
                    .uboot_config
                    .as_deref()
                    .map(|path| snapshot_path_value(self.workspace_root(), path)),
            },
        };
        let request = ResolvedStarryRequest {
            package: STARRY_PACKAGE.to_owned(),
            arch,
            target,
            smp,
            debug: cli.debug,
            source: input.source,
            config,
            runtime,
            runtime_env,
            environment_features,
            qemu_config: paths.qemu_config,
            uboot_config: paths.uboot_config,
        };
        Ok((request, snapshot))
    }

    pub(crate) fn store_starry_snapshot(
        &self,
        snapshot: &StarryCommandSnapshot,
    ) -> anyhow::Result<PathBuf> {
        snapshot.store(self.workspace_root())
    }

    pub(crate) fn prepare_axvisor_request(
        &self,
        cli: AxvisorCliArgs,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
    ) -> anyhow::Result<(ResolvedAxvisorRequest, AxvisorCommandSnapshot)> {
        let runtime_env = RuntimeEnv::capture()?;
        let environment_features = crate::build::makefile_features_from_env();
        let saved = AxvisorCommandSnapshot::load(self.workspace_root())?;
        let inherit = cli.preset.is_none()
            && cli.config.is_none()
            && cli.arch.is_none()
            && cli.target.is_none();
        let path = cli
            .config
            .as_deref()
            .or_else(|| inherit.then_some(saved.config.as_deref()).flatten());
        let preset = cli
            .preset
            .as_deref()
            .or_else(|| inherit.then_some(saved.preset.as_deref()).flatten());
        let mut input: ConfigInput<AxvisorConfig> = self.load_request_input(path, preset)?;
        input.check_selectors(cli.target.as_deref(), cli.arch.as_deref())?;
        let (arch, target) = resolve_axvisor_arch_and_target(
            cli.arch.clone().or_else(|| {
                (inherit && cli.target.is_none() && input.config.common.target.is_none())
                    .then(|| saved.arch.clone())
                    .flatten()
            }),
            cli.target
                .clone()
                .or(input.config.common.target.clone())
                .or_else(|| {
                    (inherit && cli.arch.is_none())
                        .then(|| saved.target.clone())
                        .flatten()
                }),
        )?;
        input.select_default_qemu(&target)?;
        let smp = cli.smp.or_else(|| inherit.then_some(saved.smp).flatten());
        let inherit_runtime = inherit && cli.vmconfigs.is_empty();
        let paths = self.resolve_runtime_paths(
            qemu_config,
            inherit_runtime
                .then_some(saved.qemu.qemu_config.as_ref())
                .flatten(),
            uboot_config,
            inherit_runtime
                .then_some(saved.uboot.uboot_config.as_ref())
                .flatten(),
        );
        let vmconfigs = if !cli.vmconfigs.is_empty() {
            self.resolve_workspace_paths(cli.vmconfigs.iter())
        } else if inherit_runtime {
            self.resolve_workspace_paths(saved.vmconfigs.iter())
        } else {
            Vec::new()
        };
        let mut config = input.config;
        if !vmconfigs.is_empty() {
            config.vm_configs.clone_from(&vmconfigs);
        }
        config.common.target = Some(target.clone());
        crate::build::apply_makefile_features(&mut config.common, &environment_features)?;
        if let Some(smp) = smp {
            config.common.max_cpu_num = Some(smp);
        }
        config.validate()?;
        crate::axvisor::build::validate_workspace_features(&config, self.workspace_metadata())?;
        crate::build::validate_features(&config.common)?;
        let runtime = resolve_runtime(
            config.common.max_cpu_num,
            config.common.runtime,
            &runtime_env,
        )?;
        let snapshot = AxvisorCommandSnapshot {
            preset: input.selection_preset,
            config: input
                .source
                .path()
                .map(|path| snapshot_path_value(self.workspace_root(), path)),
            arch: Some(arch.clone()),
            target: Some(target.clone()),
            smp,
            vmconfigs: vmconfigs
                .iter()
                .map(|path| snapshot_path_value(self.workspace_root(), path))
                .collect(),
            qemu: AxvisorQemuSnapshot {
                qemu_config: paths
                    .qemu_config
                    .as_deref()
                    .map(|path| snapshot_path_value(self.workspace_root(), path)),
            },
            uboot: AxvisorUbootSnapshot {
                uboot_config: paths
                    .uboot_config
                    .as_deref()
                    .map(|path| snapshot_path_value(self.workspace_root(), path)),
            },
        };
        let request = ResolvedAxvisorRequest {
            package: crate::axvisor::build::AXVISOR_PACKAGE.to_owned(),
            axvisor_dir: self.workspace_root().join("os/axvisor"),
            arch,
            target,
            smp,
            debug: cli.debug,
            source: input.source,
            config,
            runtime,
            runtime_env,
            environment_features,
            qemu_config: paths.qemu_config,
            uboot_config: paths.uboot_config,
            vmconfigs,
        };
        Ok((request, snapshot))
    }

    pub(crate) fn store_axvisor_snapshot(
        &self,
        snapshot: &AxvisorCommandSnapshot,
    ) -> anyhow::Result<PathBuf> {
        snapshot.store(self.workspace_root())
    }

    fn load_request_input<T: crate::build::BuildConfiguration>(
        &self,
        path: Option<&Path>,
        preset: Option<&str>,
    ) -> anyhow::Result<ConfigInput<T>> {
        load_config_input(self.workspace_root(), path, preset).with_context(|| {
            "resolve build selection; use `--preset <name>` or explicit `defconfig <name>` to \
             replace an obsolete saved template"
        })
    }

    fn resolve_runtime_paths(
        &self,
        qemu_config: Option<PathBuf>,
        snapshot_qemu: Option<&PathBuf>,
        uboot_config: Option<PathBuf>,
        snapshot_uboot: Option<&PathBuf>,
    ) -> ResolvedCommandPaths {
        ResolvedCommandPaths {
            qemu_config: qemu_config
                .or_else(|| resolve_snapshot_path(self.workspace_root(), snapshot_qemu)),
            uboot_config: uboot_config
                .or_else(|| resolve_snapshot_path(self.workspace_root(), snapshot_uboot)),
        }
    }

    fn resolve_workspace_paths<'a>(
        &self,
        paths: impl IntoIterator<Item = &'a PathBuf>,
    ) -> Vec<PathBuf> {
        paths
            .into_iter()
            .map(|path| self.workspace_root().join(path))
            .collect()
    }
}

pub(crate) fn resolve_snapshot_path(root: &Path, path: Option<&PathBuf>) -> Option<PathBuf> {
    path.map(|path| root.join(path))
}

pub(crate) fn snapshot_path_value(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root)
        .map(PathBuf::from)
        .unwrap_or_else(|_| path.to_path_buf())
}
