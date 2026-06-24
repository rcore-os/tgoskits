//! Shared C/Python QEMU test case build orchestration.
//!
//! Main responsibilities:
//! - Prepare guest prebuild and host cross-build environments for C cases
//! - Generate toolchain and wrapper scripts used during case builds
//! - Run prebuild scripts and CMake configure/build/install steps
//! - Populate case overlays that will later be injected into the rootfs image

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use anyhow::{Context, bail, ensure};

use super::{
    case as case_assets,
    case::{CaseAssetConfig, TestQemuCase, TestQemuSubcase, TestQemuSubcaseKind},
    timing,
};
use crate::{context::CrossCompileSpec, support::process::ProcessExt};

const CASE_C_DIR_NAME: &str = "c";
const CASE_PREBUILD_SCRIPT_NAME: &str = "prebuild.sh";
const CASE_CMAKE_FILE_NAME: &str = "CMakeLists.txt";
const CROSS_BINUTILS: &[&str] = &[
    "ld", "as", "ar", "ranlib", "strip", "nm", "objcopy", "objdump", "readelf",
];

#[derive(Debug, Clone)]
pub(crate) struct HostCrossBuildEnv {
    cmake: PathBuf,
    pkg_config: PathBuf,
    make_program: PathBuf,
    cmake_toolchain_file: PathBuf,
    command_envs: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub(crate) struct GuestPrebuildEnv {
    qemu_runner: PathBuf,
    script_envs: Vec<(String, String)>,
}

/// Returns the C source directory for a QEMU test case.
mod c;
mod cmake;
mod env;
mod grouped_c;
mod prebuild;
mod python;
mod rust;
mod toolchain;
mod wrappers;

pub(crate) use c::{case_c_source_dir, prepare_c_case_assets_sync};
use c::{
    case_rust_prebuild_script_path, grouped_c_root_project_path,
    grouped_c_subcase_prebuild_script_path, grouped_c_subcase_source_dir,
};
use cmake::{
    build_cmake_build_command, build_cmake_configure_command,
    build_cmake_configure_command_with_source_dir, build_cmake_install_command,
    build_grouped_c_root_project_configure_command, grouped_c_subcase_list,
};
use env::{prepare_guest_package_env, prepare_guest_prebuild_env, prepare_host_cross_build_env};
pub(crate) use grouped_c::prepare_grouped_case_assets_sync;
use prebuild::{build_prebuild_command, build_prebuild_command_with_work_dir};
use python::write_musl_loader_search_path;
pub(crate) use python::{case_python_source_dir, prepare_python_case_assets_sync};
pub(crate) use rust::{case_rust_source_dir, prepare_rust_case_assets_sync};
use toolchain::{cross_compile_spec, write_cmake_toolchain_file, write_cross_bin_wrappers};
use wrappers::{
    apply_case_script_envs, case_script_envs, ensure_guest_tool_exists,
    find_host_binary_candidates, guest_library_path, qemu_user_binary_names,
    write_guest_command_wrappers, write_guest_exec_wrapper,
};

#[cfg(test)]
mod tests;
