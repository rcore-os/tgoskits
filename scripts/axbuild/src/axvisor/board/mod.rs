use std::path::PathBuf;

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

pub fn find_board(name: &str) -> Option<Board> {
    board_default_list()
        .into_iter()
        .find(|board| board.name == name)
}

pub fn default_board_for_target(target: &str) -> Option<AxvisorBoardConfig> {
    let board_name = match target {
        "aarch64-unknown-none-softfloat" => "qemu-aarch64",
        "riscv64gc-unknown-none-elf" => "qemu-riscv64",
        "x86_64-unknown-none" => "qemu-x86_64",
        _ => return None,
    };
    find_board(board_name).map(|board| board.config)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_default_qemu_board_by_target() {
        let aarch64 = default_board_for_target("aarch64-unknown-none-softfloat").unwrap();
        assert!(aarch64.arceos.plat_dyn);
        assert!(aarch64.arceos.features.contains(&"ept-level-4".to_string()));
        assert!(
            aarch64
                .arceos
                .features
                .contains(&"axstd/bus-mmio".to_string())
        );

        let x86 = default_board_for_target("x86_64-unknown-none").unwrap();
        assert!(!x86.arceos.plat_dyn);
        assert!(x86.arceos.features.contains(&"ept-level-4".to_string()));
        assert!(x86.arceos.features.contains(&"fs".to_string()));

        let riscv = default_board_for_target("riscv64gc-unknown-none-elf").unwrap();
        assert!(!riscv.arceos.plat_dyn);
        assert!(riscv.arceos.features.contains(&"ept-level-4".to_string()));
        assert!(
            riscv
                .arceos
                .features
                .contains(&"axstd/bus-mmio".to_string())
        );
    }
}
