use std::path::{Path, PathBuf};

use anyhow::anyhow;

use super::{
    ARCEOS_SNAPSHOT_FILE, AppContext, ArceosCommandSnapshot, ArceosQemuSnapshot,
    ArceosUbootSnapshot, AxvisorCliArgs, AxvisorCommandSnapshot, AxvisorQemuSnapshot,
    AxvisorUbootSnapshot, BuildCliArgs, ResolvedAxvisorRequest, ResolvedBuildRequest,
    ResolvedStarryRequest, STARRY_PACKAGE, StarryCliArgs, StarryCommandSnapshot,
    StarryQemuSnapshot, StarryUbootSnapshot, resolve_arceos_arch_and_target,
    resolve_axvisor_arch_and_target, resolve_starry_arch_and_target,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ResolvedCommandPaths {
    qemu_config: Option<PathBuf>,
    uboot_config: Option<PathBuf>,
}

pub(crate) struct AxvisorRequestPaths<L, R> {
    pub(crate) package: String,
    pub(crate) axvisor_dir: PathBuf,
    pub(crate) load_config_target: L,
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
        let effective_arch = cli.arch.clone().or_else(|| {
            if cli.target.is_some() {
                None
            } else {
                snapshot.arch.clone()
            }
        });
        let effective_target = cli.target.clone().or_else(|| {
            if cli.arch.is_some() {
                None
            } else {
                snapshot.target.clone()
            }
        });
        let (arch, target) = resolve_arceos_arch_and_target(effective_arch, effective_target)?;
        let inherit_snapshot_runtime = cli.package.is_none()
            && cli.arch.is_none()
            && cli.target.is_none()
            && cli.config.is_none();
        let plat_dyn = cli.plat_dyn.or_else(|| {
            inherit_snapshot_runtime
                .then_some(snapshot.plat_dyn)
                .flatten()
        });
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
        let build_info_path = resolve_build_info_path(&package, &target, cli.config.clone())?;

        let request = ResolvedBuildRequest {
            package: package.clone(),
            arch: arch.clone(),
            target: target.clone(),
            plat_dyn,
            smp,
            debug: cli.debug,
            build_info_path,
            qemu_config: runtime_paths.qemu_config.clone(),
            uboot_config: runtime_paths.uboot_config.clone(),
        };

        let snapshot = ArceosCommandSnapshot {
            package: Some(package),
            arch: Some(arch),
            target: Some(target),
            plat_dyn,
            smp,
            qemu: ArceosQemuSnapshot {
                qemu_config: runtime_paths
                    .qemu_config
                    .as_ref()
                    .map(|path| snapshot_path_value(&self.root, path)),
            },
            uboot: ArceosUbootSnapshot {
                uboot_config: runtime_paths
                    .uboot_config
                    .as_ref()
                    .map(|path| snapshot_path_value(&self.root, path)),
            },
        };

        Ok((request, snapshot))
    }

    pub(crate) fn store_arceos_snapshot(
        &self,
        snapshot: &ArceosCommandSnapshot,
    ) -> anyhow::Result<PathBuf> {
        snapshot.store(&self.root)
    }

    pub(crate) fn prepare_starry_request(
        &self,
        cli: StarryCliArgs,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
        resolve_build_info_path: impl FnOnce(&Path, &str, Option<PathBuf>) -> anyhow::Result<PathBuf>,
    ) -> anyhow::Result<(ResolvedStarryRequest, StarryCommandSnapshot)> {
        let snapshot = StarryCommandSnapshot::load(&self.root)?;
        let inherit_snapshot_config =
            cli.config.is_none() && cli.arch.is_none() && cli.target.is_none();
        let resolved_config = self.resolve_command_path(
            cli.config.clone(),
            inherit_snapshot_config
                .then_some(snapshot.config.as_ref())
                .flatten(),
        );
        let config_target = resolved_config
            .as_deref()
            .filter(|_| cli.config.is_some() && cli.target.is_none())
            .map(crate::starry::build::load_target_from_build_config)
            .transpose()?
            .flatten();
        let effective_arch = cli.arch.clone().or_else(|| {
            if cli.target.is_some() || config_target.is_some() {
                None
            } else {
                snapshot.arch.clone()
            }
        });
        let effective_target = cli.target.clone().or(config_target).or_else(|| {
            if cli.arch.is_some() {
                None
            } else {
                snapshot.target.clone()
            }
        });
        let (arch, target) = resolve_starry_arch_and_target(effective_arch, effective_target)?;
        let smp = cli.smp.or(snapshot.smp);
        let inherit_snapshot_runtime = cli.arch.is_none() && cli.target.is_none();
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
        let build_info_path = resolve_build_info_path(&self.root, &target, resolved_config)?;

        let request = ResolvedStarryRequest {
            package: STARRY_PACKAGE.to_string(),
            arch: arch.clone(),
            target: target.clone(),
            plat_dyn: None,
            smp,
            debug: cli.debug,
            build_info_path: build_info_path.clone(),
            build_info_override: None,
            qemu_config: runtime_paths.qemu_config.clone(),
            uboot_config: runtime_paths.uboot_config.clone(),
        };

        let snapshot = StarryCommandSnapshot {
            arch: Some(arch),
            target: Some(target),
            smp,
            config: Some(snapshot_path_value(&self.root, &build_info_path)),
            qemu: StarryQemuSnapshot {
                qemu_config: runtime_paths
                    .qemu_config
                    .as_ref()
                    .map(|path| snapshot_path_value(&self.root, path)),
            },
            uboot: StarryUbootSnapshot {
                uboot_config: runtime_paths
                    .uboot_config
                    .as_ref()
                    .map(|path| snapshot_path_value(&self.root, path)),
            },
        };

        Ok((request, snapshot))
    }

    pub(crate) fn store_starry_snapshot(
        &self,
        snapshot: &StarryCommandSnapshot,
    ) -> anyhow::Result<PathBuf> {
        snapshot.store(&self.root)
    }

    pub(crate) fn prepare_axvisor_request(
        &self,
        cli: AxvisorCliArgs,
        paths: AxvisorRequestPaths<
            impl FnOnce(&Path) -> anyhow::Result<Option<String>>,
            impl FnOnce(&Path, &str, Option<PathBuf>) -> anyhow::Result<PathBuf>,
        >,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
    ) -> anyhow::Result<(ResolvedAxvisorRequest, AxvisorCommandSnapshot)> {
        let AxvisorRequestPaths {
            package,
            axvisor_dir,
            load_config_target,
            resolve_build_info_path,
        } = paths;
        let snapshot = AxvisorCommandSnapshot::load(&self.root)?;
        let inherit_snapshot_config =
            cli.config.is_none() && cli.arch.is_none() && cli.target.is_none();
        let resolved_config = self.resolve_command_path(
            cli.config.clone(),
            inherit_snapshot_config
                .then_some(snapshot.config.as_ref())
                .flatten(),
        );
        let config_target = resolved_config
            .as_ref()
            .filter(|path| path.exists())
            .map(|path| load_config_target(path))
            .transpose()?
            .flatten();
        let explicit_config = if cli.config.is_some() {
            resolved_config
        } else {
            resolved_config.filter(|path| path.exists())
        };
        let effective_arch = cli.arch.clone().or_else(|| {
            if cli.target.is_some() || config_target.is_some() {
                None
            } else {
                snapshot.arch.clone()
            }
        });
        let effective_target = cli.target.clone().or(config_target.clone()).or_else(|| {
            if cli.arch.is_some() {
                None
            } else {
                snapshot.target.clone()
            }
        });
        let (arch, target) = resolve_axvisor_arch_and_target(effective_arch, effective_target)?;
        let plat_dyn = cli.plat_dyn.or(snapshot.plat_dyn);
        let smp = cli.smp.or(snapshot.smp);
        let build_info_path = resolve_build_info_path(&axvisor_dir, &target, explicit_config)?;
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
        let inherit_snapshot_vmconfigs = inherit_snapshot_runtime;
        let vmconfigs = if cli.vmconfigs.is_empty() && inherit_snapshot_vmconfigs {
            self.resolve_workspace_paths(snapshot.vmconfigs.iter())
        } else if !cli.vmconfigs.is_empty() {
            self.resolve_workspace_paths(cli.vmconfigs.iter())
        } else {
            Vec::new()
        };

        let request = ResolvedAxvisorRequest {
            package,
            axvisor_dir,
            arch: arch.clone(),
            target: target.clone(),
            plat_dyn,
            smp,
            debug: cli.debug,
            build_info_path: build_info_path.clone(),
            qemu_config: runtime_paths.qemu_config.clone(),
            uboot_config: runtime_paths.uboot_config.clone(),
            vmconfigs: vmconfigs.clone(),
        };

        let snapshot = AxvisorCommandSnapshot {
            arch: Some(arch),
            target: Some(target),
            plat_dyn,
            smp,
            config: Some(snapshot_path_value(&self.root, &build_info_path)),
            vmconfigs: vmconfigs
                .iter()
                .map(|path| snapshot_path_value(&self.root, path))
                .collect(),
            qemu: AxvisorQemuSnapshot {
                qemu_config: runtime_paths
                    .qemu_config
                    .as_ref()
                    .map(|path| snapshot_path_value(&self.root, path)),
            },
            uboot: AxvisorUbootSnapshot {
                uboot_config: runtime_paths
                    .uboot_config
                    .as_ref()
                    .map(|path| snapshot_path_value(&self.root, path)),
            },
        };

        Ok((request, snapshot))
    }

    pub(crate) fn store_axvisor_snapshot(
        &self,
        snapshot: &AxvisorCommandSnapshot,
    ) -> anyhow::Result<PathBuf> {
        snapshot.store(&self.root)
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
        explicit_path.or_else(|| resolve_snapshot_path(&self.root, snapshot_path))
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
            self.root.join(path)
        }
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
