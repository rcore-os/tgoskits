use std::path::{Path, PathBuf};

use ostool::{build::config::Cargo, run::qemu::QemuConfig};

use crate::{
    context::ResolvedBuildRequest,
    test::{board as board_test, case::TestQemuCase, qemu as qemu_test},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum QemuTestFlow {
    Rust,
    C,
    Axtest,
    Generic(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ArceosRustQemuCase {
    pub(super) case: TestQemuCase,
    pub(super) build_group: String,
    pub(super) build_config_path: PathBuf,
    pub(super) package: String,
    pub(super) feature: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct PreparedArceosRustQemuCase {
    pub(super) case: ArceosRustQemuCase,
    pub(super) request: ResolvedBuildRequest,
    pub(super) cargo: Cargo,
    pub(super) qemu: QemuConfig,
    pub(super) host_symbolize_success_regex: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArceosBoardTestGroup {
    pub(crate) name: String,
    pub(crate) board_name: String,
    pub(crate) package: String,
    pub(crate) arch: String,
    pub(crate) target: String,
    pub(crate) build_config_path: PathBuf,
    pub(crate) board_test_config_path: PathBuf,
    pub(crate) is_axtest: bool,
}

impl board_test::BoardTestGroupInfo for ArceosBoardTestGroup {
    fn name(&self) -> &str {
        &self.name
    }

    fn board_name(&self) -> &str {
        &self.board_name
    }
}

impl qemu_test::BuildConfigRef for PreparedArceosRustQemuCase {
    fn build_group(&self) -> &str {
        &self.case.build_group
    }

    fn build_config_path(&self) -> &Path {
        &self.case.build_config_path
    }
}

pub(super) struct ArceosQemuBuildGroup<'a> {
    pub(super) build_group: &'a str,
    pub(super) build_config_path: &'a Path,
    pub(super) package: &'a str,
    pub(super) feature: Option<&'a str>,
    pub(super) request: ResolvedBuildRequest,
    pub(super) cargo: Cargo,
    pub(super) cases: Vec<&'a PreparedArceosRustQemuCase>,
}

pub(super) struct GenericQemuRunOptions<'a> {
    pub(super) selected_case: Option<&'a str>,
    pub(super) symbolize_after: bool,
    pub(super) keep_qemu_log: bool,
    pub(super) allow_empty: bool,
}

/// A discovered C test under `test-suit/arceos/c/`.
pub(super) struct CTestDef {
    pub(super) name: String,
    pub(super) build_group: String,
    pub(super) build_config_path: PathBuf,
    pub(super) qemu_config_path: PathBuf,
}

impl qemu_test::BuildConfigRef for CTestDef {
    fn build_group(&self) -> &str {
        &self.build_group
    }

    fn build_config_path(&self) -> &Path {
        &self.build_config_path
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CTestArtifactPaths {
    pub(super) target_dir: PathBuf,
    pub(super) out_dir: PathBuf,
}
