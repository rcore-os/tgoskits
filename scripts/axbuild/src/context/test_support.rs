//! Fixture construction for callers testing behavior after request resolution.

use std::path::Path;

use ax_config::{RuntimeEnv, resolve_runtime};

use super::{ResolvedAxvisorRequest, ResolvedBuildRequest, ResolvedStarryRequest};
use crate::build::ConfigSource;

pub(crate) fn arceos_request(
    path: &Path,
    package: &str,
    arch: &str,
    target: &str,
) -> ResolvedBuildRequest {
    let mut config: crate::arceos::config::ArceosConfig = if path.is_file() {
        crate::build::load_build_config(path).unwrap()
    } else {
        Default::default()
    };
    config.package = Some(package.to_string());
    config.common.target = Some(target.to_string());
    let runtime_env = RuntimeEnv::default();
    let runtime = resolve_runtime(
        config.common.max_cpu_num,
        config.common.runtime,
        &runtime_env,
    )
    .unwrap();
    ResolvedBuildRequest {
        package: package.to_string(),
        arch: arch.to_string(),
        target: target.to_string(),
        smp: None,
        debug: false,
        source: ConfigSource::File {
            path: path.to_path_buf(),
            preset: None,
        },
        config,
        runtime,
        runtime_env,
        environment_features: Vec::new(),
        qemu_config: None,
        uboot_config: None,
    }
}

pub(crate) fn starry_request(path: &Path, arch: &str, target: &str) -> ResolvedStarryRequest {
    let mut config: crate::starry::config::StarryConfig = if path.is_file() {
        crate::build::load_build_config(path).unwrap()
    } else {
        Default::default()
    };
    config.common.target = Some(target.to_string());
    let runtime_env = RuntimeEnv::default();
    let runtime = resolve_runtime(
        config.common.max_cpu_num,
        config.common.runtime,
        &runtime_env,
    )
    .unwrap();
    ResolvedStarryRequest {
        package: super::STARRY_PACKAGE.to_string(),
        arch: arch.to_string(),
        target: target.to_string(),
        smp: None,
        debug: false,
        source: ConfigSource::File {
            path: path.to_path_buf(),
            preset: None,
        },
        config,
        runtime,
        runtime_env,
        environment_features: Vec::new(),
        qemu_config: None,
        uboot_config: None,
    }
}

pub(crate) fn axvisor_request(
    path: &Path,
    arch: &str,
    target: &str,
    axvisor_dir: &Path,
) -> ResolvedAxvisorRequest {
    let mut config: crate::axvisor::config::AxvisorConfig = if path.is_file() {
        crate::build::load_build_config(path).unwrap()
    } else {
        Default::default()
    };
    config.common.target = Some(target.to_string());
    let runtime_env = RuntimeEnv::default();
    let runtime = resolve_runtime(
        config.common.max_cpu_num,
        config.common.runtime,
        &runtime_env,
    )
    .unwrap();
    ResolvedAxvisorRequest {
        package: crate::axvisor::build::AXVISOR_PACKAGE.to_string(),
        axvisor_dir: axvisor_dir.to_path_buf(),
        arch: arch.to_string(),
        target: target.to_string(),
        smp: None,
        debug: false,
        source: ConfigSource::File {
            path: path.to_path_buf(),
            preset: None,
        },
        config,
        runtime,
        runtime_env,
        environment_features: Vec::new(),
        qemu_config: None,
        uboot_config: None,
        vmconfigs: vec![],
    }
}
