use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};
use ax_config::{RuntimeEnv, resolve_runtime};

use super::{
    ARCEOS_SNAPSHOT_FILE, AppContext, ArceosCommandSnapshot, ArceosQemuSnapshot,
    ArceosUbootSnapshot, AxvisorCliArgs, AxvisorCommandSnapshot, AxvisorQemuSnapshot,
    AxvisorUbootSnapshot, BuildCliArgs, ResolvedAxvisorRequest, ResolvedBuildRequest,
    ResolvedStarryRequest, STARRY_PACKAGE, StarryCliArgs, StarryCommandSnapshot,
    StarryQemuSnapshot, StarryUbootSnapshot, resolve_arceos_arch_and_target,
    resolve_axvisor_arch_and_target, resolve_starry_arch_and_target,
};
use crate::{
    arceos::{cbuild::AX_LIBC_PACKAGE, config::ArceosConfig},
    axvisor::config::AxvisorConfig,
    starry::config::StarryConfig,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ResolvedCommandPaths {
    qemu_config: Option<PathBuf>,
    uboot_config: Option<PathBuf>,
}

pub(crate) struct AxvisorRequestPaths<R> {
    pub(crate) package: String,
    pub(crate) axvisor_dir: PathBuf,
    pub(crate) resolve_build_info_path: R,
}

impl AppContext {
    pub(crate) fn prepare_arceos_request(
        &self,
        cli: BuildCliArgs,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
        resolve_build_info_path: impl FnOnce(&str, &str, Option<PathBuf>) -> anyhow::Result<PathBuf>,
    ) -> anyhow::Result<(ResolvedBuildRequest, ArceosCommandSnapshot)> {
        self.prepare_arceos_request_with_input_check(
            cli,
            qemu_config,
            uboot_config,
            resolve_build_info_path,
            |_| Ok(()),
        )
    }

    /// Check unselected file input before any selector defaults or snapshot
    /// values are applied. No selected file is represented by an empty input.
    pub(crate) fn prepare_arceos_request_with_input_check(
        &self,
        cli: BuildCliArgs,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
        resolve_build_info_path: impl FnOnce(&str, &str, Option<PathBuf>) -> anyhow::Result<PathBuf>,
        input_check: impl FnOnce(&ArceosConfig) -> anyhow::Result<()>,
    ) -> anyhow::Result<(ResolvedBuildRequest, ArceosCommandSnapshot)> {
        let runtime_env = RuntimeEnv::capture()?;
        let environment_features = crate::build::makefile_features_from_env();
        let snapshot = ArceosCommandSnapshot::load(self.workspace_root())?;
        let inherit_snapshot_config = cli.package.is_none()
            && cli.arch.is_none()
            && cli.target.is_none()
            && cli.config.is_none();
        let resolved_config = self.resolve_command_path(
            cli.config.clone(),
            inherit_snapshot_config
                .then_some(snapshot.config.as_ref())
                .flatten(),
        );
        let mut config: Option<ArceosConfig> =
            load_selected_config(resolved_config.as_deref(), cli.config.is_some())?;
        match config.as_ref() {
            Some(input) => input_check(input)?,
            None => input_check(&ArceosConfig::default())?,
        }
        let config_package = config.as_ref().and_then(|config| config.package.clone());
        let config_target = config
            .as_ref()
            .and_then(|config| config.common.target.clone());
        let uses_app_c = config.as_ref().is_some_and(|config| config.app_c.is_some());
        let explicit_config_path = if cli.config.is_some() {
            resolved_config
        } else {
            resolved_config.filter(|path| path.exists())
        };

        // Select the winning input before checking C application's ax-libc
        // contract. A CLI package may replace an invalid draft file value.
        let selected_package = cli.package.clone().or(config_package);
        let package = if uses_app_c {
            selected_package.unwrap_or_else(|| AX_LIBC_PACKAGE.to_string())
        } else {
            selected_package
                .or(snapshot.package.clone())
                .ok_or_else(|| {
                    anyhow!(
                        "missing ArceOS package; pass `--package` or set `package` in {}",
                        ARCEOS_SNAPSHOT_FILE
                    )
                })?
        };
        let effective_arch = cli.arch.clone().or_else(|| {
            if cli.target.is_some() || config_target.is_some() {
                None
            } else {
                inherit_snapshot_config
                    .then(|| snapshot.arch.clone())
                    .flatten()
            }
        });
        let effective_target = cli.target.clone().or(config_target).or_else(|| {
            if cli.arch.is_some() {
                None
            } else {
                inherit_snapshot_config
                    .then(|| snapshot.target.clone())
                    .flatten()
            }
        });
        let (arch, target) = resolve_arceos_arch_and_target(effective_arch, effective_target)?;
        let inherit_snapshot_runtime = cli.package.is_none()
            && cli.arch.is_none()
            && cli.target.is_none()
            && cli.config.is_none();
        let smp = cli
            .smp
            .or_else(|| inherit_snapshot_runtime.then_some(snapshot.smp).flatten());
        let runtime_paths = self.resolve_runtime_paths(
            qemu_config,
            if inherit_snapshot_runtime {
                snapshot.qemu.qemu_config.as_ref()
            } else {
                None
            },
            uboot_config,
            if inherit_snapshot_runtime {
                snapshot.uboot.uboot_config.as_ref()
            } else {
                None
            },
        );
        let build_info_path = resolve_build_info_path(&package, &target, explicit_config_path)?;
        let mut config = match config.take() {
            Some(config) => config,
            None => {
                if build_info_path.exists() {
                    crate::build::load_build_config(&build_info_path)?
                } else {
                    let board = if self
                        .workspace_root()
                        .join("os/arceos/configs/board")
                        .exists()
                    {
                        crate::arceos::config::ensure_default_build_config_for_target(
                            self.workspace_root(),
                            &package,
                            &target,
                            &build_info_path,
                        )?
                    } else {
                        None
                    };
                    let config = board.map(|board| board.build_config).unwrap_or_default();
                    crate::build::ensure_build_info(&build_info_path, || config.clone())?;
                    config
                }
            }
        };
        config.package = Some(package.clone());
        config.common.target = Some(target.clone());
        crate::build::apply_makefile_features(&mut config.common, &environment_features)?;
        if let Some(smp) = smp {
            config.common.max_cpu_num = Some(smp);
        }
        config
            .validate()
            .with_context(|| format!("invalid ArceOS config {}", build_info_path.display()))?;
        if let Some(app_c) = &config.app_c {
            config.app_c = Some(crate::arceos::build::resolve_app_c_dir(
                &build_info_path,
                app_c,
            )?);
        }
        crate::build::validate_features(&config.common)?;
        let runtime = resolve_runtime(
            config.common.max_cpu_num,
            config.common.runtime,
            &runtime_env,
        )?;

        let request = ResolvedBuildRequest {
            package: package.clone(),
            arch: arch.clone(),
            target: target.clone(),
            smp,
            debug: cli.debug,
            build_info_path: build_info_path.clone(),
            config,
            runtime,
            runtime_env,
            qemu_config: runtime_paths.qemu_config.clone(),
            uboot_config: runtime_paths.uboot_config.clone(),
        };

        let snapshot = ArceosCommandSnapshot {
            package: Some(package),
            arch: Some(arch),
            target: Some(target),
            smp,
            config: Some(snapshot_path_value(self.workspace_root(), &build_info_path)),
            qemu: ArceosQemuSnapshot {
                qemu_config: runtime_paths
                    .qemu_config
                    .as_ref()
                    .map(|path| snapshot_path_value(self.workspace_root(), path)),
            },
            uboot: ArceosUbootSnapshot {
                uboot_config: runtime_paths
                    .uboot_config
                    .as_ref()
                    .map(|path| snapshot_path_value(self.workspace_root(), path)),
            },
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
        resolve_build_info_path: impl FnOnce(&Path, &str, Option<PathBuf>) -> anyhow::Result<PathBuf>,
    ) -> anyhow::Result<(ResolvedStarryRequest, StarryCommandSnapshot)> {
        let runtime_env = RuntimeEnv::capture()?;
        let environment_features = crate::build::makefile_features_from_env();
        let snapshot = StarryCommandSnapshot::load(self.workspace_root())?;
        let inherit_snapshot_config =
            cli.config.is_none() && cli.arch.is_none() && cli.target.is_none();
        let resolved_config = self.resolve_command_path(
            cli.config.clone(),
            inherit_snapshot_config
                .then_some(snapshot.config.as_ref())
                .flatten(),
        );
        let mut config: Option<StarryConfig> =
            load_selected_config(resolved_config.as_deref(), cli.config.is_some())?;
        let config_target = config
            .as_ref()
            .and_then(|config| config.common.target.clone());
        let effective_arch = cli.arch.clone().or_else(|| {
            if cli.target.is_some() || config_target.is_some() {
                None
            } else {
                inherit_snapshot_config
                    .then(|| snapshot.arch.clone())
                    .flatten()
            }
        });
        let effective_target = cli.target.clone().or(config_target).or_else(|| {
            if cli.arch.is_some() {
                None
            } else {
                inherit_snapshot_config
                    .then(|| snapshot.target.clone())
                    .flatten()
            }
        });
        let (arch, target) = resolve_starry_arch_and_target(effective_arch, effective_target)?;
        let inherit_snapshot_runtime =
            cli.config.is_none() && cli.arch.is_none() && cli.target.is_none();
        let smp = cli
            .smp
            .or_else(|| inherit_snapshot_runtime.then_some(snapshot.smp).flatten());
        let runtime_paths = self.resolve_runtime_paths(
            qemu_config,
            if inherit_snapshot_runtime {
                snapshot.qemu.qemu_config.as_ref()
            } else {
                None
            },
            uboot_config,
            if inherit_snapshot_runtime {
                snapshot.uboot.uboot_config.as_ref()
            } else {
                None
            },
        );
        let build_info_path =
            resolve_build_info_path(self.workspace_root(), &target, resolved_config)?;
        let mut config = match config.take() {
            Some(config) => config,
            None => {
                if build_info_path.exists() {
                    crate::build::load_build_config(&build_info_path)?
                } else {
                    let board = if self
                        .workspace_root()
                        .join("os/StarryOS/configs/board")
                        .exists()
                    {
                        crate::starry::config::ensure_default_build_config_for_target(
                            self.workspace_root(),
                            &target,
                            &build_info_path,
                        )?
                    } else {
                        None
                    };
                    let config = board.map(|board| board.config).unwrap_or_default();
                    crate::build::ensure_build_info(&build_info_path, || config.clone())?;
                    config
                }
            }
        };
        config.common.target = Some(target.clone());
        crate::build::apply_makefile_features(&mut config.common, &environment_features)?;
        if let Some(smp) = smp {
            config.common.max_cpu_num = Some(smp);
        }
        config
            .validate()
            .with_context(|| format!("invalid Starry config {}", build_info_path.display()))?;
        crate::build::validate_features(&config.common)?;
        config.validate_resources(self.workspace_root())?;
        let runtime = resolve_runtime(
            config.common.max_cpu_num,
            config.common.runtime,
            &runtime_env,
        )?;

        let request = ResolvedStarryRequest {
            package: STARRY_PACKAGE.to_string(),
            arch: arch.clone(),
            target: target.clone(),
            smp,
            debug: cli.debug,
            build_info_path: build_info_path.clone(),
            config,
            runtime,
            runtime_env,
            environment_features,
            qemu_config: runtime_paths.qemu_config.clone(),
            uboot_config: runtime_paths.uboot_config.clone(),
        };

        let snapshot = StarryCommandSnapshot {
            arch: Some(arch),
            target: Some(target),
            smp,
            config: Some(snapshot_path_value(self.workspace_root(), &build_info_path)),
            qemu: StarryQemuSnapshot {
                qemu_config: runtime_paths
                    .qemu_config
                    .as_ref()
                    .map(|path| snapshot_path_value(self.workspace_root(), path)),
            },
            uboot: StarryUbootSnapshot {
                uboot_config: runtime_paths
                    .uboot_config
                    .as_ref()
                    .map(|path| snapshot_path_value(self.workspace_root(), path)),
            },
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
        paths: AxvisorRequestPaths<
            impl FnOnce(&Path, &str, Option<PathBuf>) -> anyhow::Result<PathBuf>,
        >,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
    ) -> anyhow::Result<(ResolvedAxvisorRequest, AxvisorCommandSnapshot)> {
        let AxvisorRequestPaths {
            package,
            axvisor_dir,
            resolve_build_info_path,
        } = paths;
        let runtime_env = RuntimeEnv::capture()?;
        let environment_features = crate::build::makefile_features_from_env();
        let snapshot = AxvisorCommandSnapshot::load(self.workspace_root())?;
        let inherit_snapshot_config =
            cli.config.is_none() && cli.arch.is_none() && cli.target.is_none();
        let resolved_config = self.resolve_command_path(
            cli.config.clone(),
            inherit_snapshot_config
                .then_some(snapshot.config.as_ref())
                .flatten(),
        );
        let mut config: Option<AxvisorConfig> =
            load_selected_config(resolved_config.as_deref(), cli.config.is_some())?;
        let config_target = config
            .as_ref()
            .and_then(|config| config.common.target.clone());
        let explicit_config = if cli.config.is_some() {
            resolved_config
        } else {
            resolved_config.filter(|path| path.exists())
        };
        let effective_arch = cli.arch.clone().or_else(|| {
            if cli.target.is_some() || config_target.is_some() {
                None
            } else {
                inherit_snapshot_config
                    .then(|| snapshot.arch.clone())
                    .flatten()
            }
        });
        let effective_target = cli.target.clone().or(config_target.clone()).or_else(|| {
            if cli.arch.is_some() {
                None
            } else {
                inherit_snapshot_config
                    .then(|| snapshot.target.clone())
                    .flatten()
            }
        });
        let (arch, target) = resolve_axvisor_arch_and_target(effective_arch, effective_target)?;
        let smp = cli
            .smp
            .or_else(|| inherit_snapshot_config.then_some(snapshot.smp).flatten());
        let build_info_path = resolve_build_info_path(&axvisor_dir, &target, explicit_config)?;
        let mut config = match config.take() {
            Some(config) => config,
            None => crate::axvisor::build::load_or_create_default_config(
                &axvisor_dir,
                &target,
                &build_info_path,
            )?,
        };
        config.common.target = Some(target.clone());
        crate::build::apply_makefile_features(&mut config.common, &environment_features)?;
        if let Some(smp) = smp {
            config.common.max_cpu_num = Some(smp);
        }
        config
            .validate()
            .with_context(|| format!("invalid Axvisor config {}", build_info_path.display()))?;
        crate::build::validate_features(&config.common)?;
        let runtime = resolve_runtime(
            config.common.max_cpu_num,
            config.common.runtime,
            &runtime_env,
        )?;

        let inherit_snapshot_runtime = cli.arch.is_none()
            && cli.target.is_none()
            && cli.config.is_none()
            && cli.vmconfigs.is_empty();
        let runtime_paths = self.resolve_runtime_paths(
            qemu_config,
            if inherit_snapshot_runtime {
                snapshot.qemu.qemu_config.as_ref()
            } else {
                None
            },
            uboot_config,
            if inherit_snapshot_runtime {
                snapshot.uboot.uboot_config.as_ref()
            } else {
                None
            },
        );
        let vmconfigs = if !cli.vmconfigs.is_empty() {
            self.resolve_workspace_paths(cli.vmconfigs.iter())
        } else if inherit_snapshot_runtime {
            self.resolve_workspace_paths(snapshot.vmconfigs.iter())
        } else {
            Vec::new()
        };

        let request = ResolvedAxvisorRequest {
            package,
            axvisor_dir,
            arch: arch.clone(),
            target: target.clone(),
            smp,
            debug: cli.debug,
            build_info_path: build_info_path.clone(),
            config,
            runtime,
            runtime_env,
            environment_features,
            qemu_config: runtime_paths.qemu_config.clone(),
            uboot_config: runtime_paths.uboot_config.clone(),
            vmconfigs: vmconfigs.clone(),
        };

        let snapshot = AxvisorCommandSnapshot {
            arch: Some(arch),
            target: Some(target),
            smp,
            config: Some(snapshot_path_value(self.workspace_root(), &build_info_path)),
            vmconfigs: vmconfigs
                .iter()
                .map(|path| snapshot_path_value(self.workspace_root(), path))
                .collect(),
            qemu: AxvisorQemuSnapshot {
                qemu_config: runtime_paths
                    .qemu_config
                    .as_ref()
                    .map(|path| snapshot_path_value(self.workspace_root(), path)),
            },
            uboot: AxvisorUbootSnapshot {
                uboot_config: runtime_paths
                    .uboot_config
                    .as_ref()
                    .map(|path| snapshot_path_value(self.workspace_root(), path)),
            },
        };

        Ok((request, snapshot))
    }

    pub(crate) fn store_axvisor_snapshot(
        &self,
        snapshot: &AxvisorCommandSnapshot,
    ) -> anyhow::Result<PathBuf> {
        snapshot.store(self.workspace_root())
    }

    fn resolve_runtime_paths(
        &self,
        qemu_config: Option<PathBuf>,
        snapshot_qemu: Option<&PathBuf>,
        uboot_config: Option<PathBuf>,
        snapshot_uboot: Option<&PathBuf>,
    ) -> ResolvedCommandPaths {
        ResolvedCommandPaths {
            qemu_config: self.resolve_command_path(qemu_config, snapshot_qemu),
            uboot_config: self.resolve_command_path(uboot_config, snapshot_uboot),
        }
    }

    fn resolve_command_path(
        &self,
        explicit_path: Option<PathBuf>,
        snapshot_path: Option<&PathBuf>,
    ) -> Option<PathBuf> {
        explicit_path.or_else(|| resolve_snapshot_path(self.workspace_root(), snapshot_path))
    }

    fn resolve_workspace_paths<'a>(
        &self,
        paths: impl IntoIterator<Item = &'a PathBuf>,
    ) -> Vec<PathBuf> {
        paths
            .into_iter()
            .map(|path| self.resolve_workspace_path(path))
            .collect()
    }

    fn resolve_workspace_path(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.workspace_root().join(path)
        }
    }
}

fn load_selected_config<T: crate::build::BuildConfiguration>(
    path: Option<&Path>,
    explicit: bool,
) -> anyhow::Result<Option<T>> {
    match path {
        Some(path) if explicit || path.exists() => crate::build::load_build_config(path).map(Some),
        _ => Ok(None),
    }
}

pub(crate) fn resolve_snapshot_path(root: &Path, path: Option<&PathBuf>) -> Option<PathBuf> {
    path.map(|path| {
        if path.is_relative() {
            root.join(path)
        } else {
            path.clone()
        }
    })
}

pub(crate) fn snapshot_path_value(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.strip_prefix(root)
            .map(PathBuf::from)
            .unwrap_or_else(|_| path.to_path_buf())
    } else {
        path.to_path_buf()
    }
}
