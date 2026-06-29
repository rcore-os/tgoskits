use std::path::{Path, PathBuf};

use ostool::run::qemu::QemuConfig;

use crate::test::{board as board_test, case::TestQemuCase, qemu as test_qemu};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AxvisorQemuCase {
    pub(crate) case: TestQemuCase,
    pub(crate) build_group: String,
    pub(crate) build_config_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PreparedAxvisorQemuCase {
    pub(super) case: AxvisorQemuCase,
    pub(super) qemu: QemuConfig,
}

impl test_qemu::BuildConfigRef for PreparedAxvisorQemuCase {
    fn build_group(&self) -> &str {
        &self.case.build_group
    }

    fn build_config_path(&self) -> &Path {
        &self.case.build_config_path
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoardTestGroup {
    pub(crate) name: String,
    pub(crate) board_name: String,
    pub(crate) build_config: PathBuf,
    pub(crate) board_test_config_path: PathBuf,
}

impl board_test::BoardTestGroupInfo for BoardTestGroup {
    fn name(&self) -> &str {
        &self.name
    }

    fn board_name(&self) -> &str {
        &self.board_name
    }
}
