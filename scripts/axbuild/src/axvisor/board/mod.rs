use std::{
    ops::{Deref, DerefMut},
    path::PathBuf,
};

use crate::{arceos::build::ArceosBuildInfo, axvisor::build::AxvisorBoardConfig};

pub struct Board {
    pub name: &'static str,
    pub config: AxvisorBoardConfig,
}

pub fn board_default_list() -> Vec<Board> {
    vec![
        Board::new("qemu-aarch64")
            .with_plat_dyn(true)
            .with_features(["ept-level-4", "axstd/bus-mmio"]),
        Board::new("qemu-riscv64")
            .with_plat_dyn(false)
            .with_features(["ept-level-4", "axstd/bus-mmio"]),
        Board::new("qemu-x86_64")
            .with_plat_dyn(false)
            .with_features(["ept-level-4", "fs"]),
    ]
}

impl Board {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            config: AxvisorBoardConfig {
                arceos: ArceosBuildInfo::default(),
                vm_configs: vec![],
            },
        }
    }

    pub fn with_plat_dyn(mut self, plat_dyn: bool) -> Self {
        self.config.arceos.plat_dyn = plat_dyn;
        self
    }

    pub fn with_vm_configs(mut self, vm_configs: Vec<PathBuf>) -> Self {
        self.config.vm_configs = vm_configs;
        self
    }

    pub fn with_features<T: AsRef<str>>(mut self, features: impl AsRef<[T]>) -> Self {
        self.config.arceos = self.config.arceos.with_features(features);
        self
    }
}
