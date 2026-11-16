use std::path::{Path, PathBuf};

use ostool::{build::config::Cargo, run::qemu::QemuConfig};

use crate::{
    arceos::config::ArceosConfig,
    build::ConfigInput,
    context::ResolvedBuildRequest,
    execution::{board as board_test, case::TestQemuCase, qemu as qemu_test},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum QemuTestFlow {
    Rust,
    C,
    Generic(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArceosQemuCase {
    pub(crate) case: TestQemuCase,
    pub(crate) build_group: String,
    pub(crate) build_config_path: PathBuf,
    pub(crate) package: String,
    /// Rust selection label: a test ID or a declared execution batch ID.
    pub(crate) feature: Option<String>,
    /// Actual Cargo roots, which may contain several features for one boot.
    pub(crate) features: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedArceosQemuCase {
    pub(crate) files: crate::build::BuildFiles,
    pub(crate) case: ArceosQemuCase,
    pub(crate) request: ResolvedBuildRequest,
    pub(crate) cargo: Cargo,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedArceosQemuCase {
    pub(crate) case: ArceosQemuCase,
    pub(crate) request: ResolvedBuildRequest,
    pub(crate) cargo: Cargo,
    pub(crate) qemu: QemuConfig,
    pub(crate) verify_pcm_output: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ArceosBoardTestGroup {
    pub(crate) name: String,
    pub(crate) board_name: String,
    pub(crate) package: String,
    pub(crate) arch: String,
    pub(crate) target: String,
    pub(crate) build_config_path: PathBuf,
    pub(crate) build_input: ConfigInput<ArceosConfig>,
    pub(crate) board_test_config_path: PathBuf,
    pub(crate) declaration: Option<super::board::selection::BoardSelection>,
}

impl board_test::BoardTestGroupInfo for ArceosBoardTestGroup {
    fn name(&self) -> &str {
        &self.name
    }

    fn board_name(&self) -> &str {
        &self.board_name
    }
}

impl qemu_test::BuildConfigRef for PreparedArceosQemuCase {
    fn build_group(&self) -> &str {
        &self.case.build_group
    }

    fn build_config_path(&self) -> &Path {
        &self.case.build_config_path
    }
}

pub(crate) struct ArceosQemuBuildGroup<'a> {
    pub(crate) build_group: &'a str,
    pub(crate) build_config_path: &'a Path,
    pub(crate) package: &'a str,
    pub(crate) feature: Option<&'a str>,
    pub(crate) features: &'a [String],
    pub(crate) request: ResolvedBuildRequest,
    pub(crate) cargo: Cargo,
    pub(crate) cases: Vec<&'a PreparedArceosQemuCase>,
}

pub(crate) struct GenericQemuRunOptions<'a> {
    pub(crate) selected_case: Option<&'a str>,
    pub(crate) allow_empty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CTestArtifactPaths {
    pub(crate) target_dir: PathBuf,
    pub(crate) out_dir: PathBuf,
}
