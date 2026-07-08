use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, bail};
use ostool::{build::config::Cargo, run::qemu::QemuConfig};
use serde::Deserialize;

use crate::{
    context::validate_supported_target,
    test::case::{HostHttpServerConfig, TestQemuCase, TestQemuSubcase, TestQemuSubcaseKind},
};

const DYNAMIC_X86_64_QEMU_DEBUG_ENV: &str = "AXBUILD_X86_64_DYN_QEMU_DEBUG";
const TIMEOUT_SCALE_ENV: &str = "AXBUILD_TEST_TIMEOUT_SCALE";

mod boot;
mod config;
mod discovery;
mod grouping;
mod summary;
mod target;
mod tree;
mod types;

pub(crate) use boot::{
    apply_drive_snapshot_without_global_snapshot, apply_dynamic_platform_qemu_boot,
    apply_smp_qemu_arg, apply_timeout_scale, qemu_timeout_summary, smp_from_qemu_arg,
};
pub(crate) use config::{
    load_qemu_case_extra_config, load_qemu_case_host_http_server, load_test_qemu_case_fields,
    validate_grouped_qemu_commands,
};
pub(crate) use discovery::{
    case_name_from_wrapper, discover_all_qemu_cases, discover_all_qemu_cases_with_archs,
    discover_qemu_cases, discover_qemu_cases_allow_empty, nearest_build_wrapper, qemu_config_name,
};
#[cfg(test)]
pub(crate) use grouping::group_cases_by_build_config;
pub(crate) use grouping::prepare_case_build_groups;
pub(crate) use summary::QemuTestSummary;
pub(crate) use target::parse_test_target;
pub(crate) use tree::{render_case_tree, render_labeled_case_forest, render_qemu_case_forest};
pub(crate) use types::{
    BuildConfigRef, DiscoveredQemuCase, ListQemuCasesError, ListQemuCasesErrorKind,
    ListQemuCasesResult, ListedQemuCase, QemuCaseBuildGroup, QemuCaseExtraConfig, QemuCaseGroup,
    TestBuildWrapper,
};

#[cfg(test)]
mod tests;
