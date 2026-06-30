#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StarryQemuCaseOutcome {
    Passed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StarryQemuCaseReport {
    pub(crate) name: String,
    pub(crate) outcome: StarryQemuCaseOutcome,
    pub(crate) duration: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StarryQemuRunReport {
    pub(crate) cases: Vec<StarryQemuCaseReport>,
    pub(crate) total_duration: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StarryBoardTestGroup {
    pub(crate) name: String,
    pub(crate) board_name: String,
    pub(crate) arch: String,
    pub(crate) target: String,
    pub(crate) build_config_path: PathBuf,
    pub(crate) board_test_config_path: PathBuf,
}

impl board_test::BoardTestGroupInfo for StarryBoardTestGroup {
    fn name(&self) -> &str {
        &self.name
    }

    fn board_name(&self) -> &str {
        &self.board_name
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StarryQemuCaseRequirements {
    pub(crate) smp: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StarryQemuCase {
    pub(crate) case: TestQemuCase,
    pub(crate) build_group: String,
    pub(crate) build_config_path: PathBuf,
}

impl qemu_test::BuildConfigRef for StarryQemuCase {
    fn build_group(&self) -> &str {
        &self.build_group
    }

    fn build_config_path(&self) -> &Path {
        &self.build_config_path
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedStarryQemuCase {
    pub(crate) case: TestQemuCase,
    pub(crate) qemu: QemuConfig,
    pub(crate) build_group: String,
    pub(crate) build_config_path: PathBuf,
    pub(crate) rootfs_path: PathBuf,
    pub(crate) requirements: StarryQemuCaseRequirements,
}

impl qemu_test::BuildConfigRef for PreparedStarryQemuCase {
    fn build_group(&self) -> &str {
        &self.build_group
    }

    fn build_config_path(&self) -> &Path {
        &self.build_config_path
    }
}
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use ostool::run::qemu::QemuConfig;

use crate::test::{board as board_test, case::TestQemuCase, qemu as qemu_test};
