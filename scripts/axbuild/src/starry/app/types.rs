use std::path::PathBuf;

use clap::ValueEnum;

use crate::test::case::{HostHttpServerConfig, TestQemuSubcase};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum StarryAppKind {
    Qemu,
    Board,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StarryAppCase {
    pub(crate) name: String,
    pub(crate) kind: StarryAppKind,
    pub(crate) case_dir: PathBuf,
    pub(crate) prebuild_path: Option<PathBuf>,
    pub(crate) requires: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StarryAppBoardCase {
    pub(crate) name: String,
    pub(crate) case_dir: PathBuf,
    pub(crate) init_path: PathBuf,
    pub(crate) init_cmd: String,
    pub(crate) build_config_path: PathBuf,
    pub(crate) board_config_path: PathBuf,
    pub(crate) target: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StarryAppQemuCase {
    pub(crate) name: String,
    pub(crate) arch: String,
    pub(crate) target: String,
    pub(crate) build_config_path: Option<PathBuf>,
    pub(crate) qemu_config_path: Option<PathBuf>,
    pub(crate) rootfs_path: PathBuf,
    pub(crate) snapshot: bool,
    pub(crate) test_commands: Vec<String>,
    pub(crate) host_symbolize_success_regex: Vec<String>,
    pub(crate) host_http_server: Option<HostHttpServerConfig>,
    pub(crate) subcases: Vec<TestQemuSubcase>,
}
