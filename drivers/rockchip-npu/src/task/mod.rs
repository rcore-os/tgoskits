use alloc::vec::Vec;
use dma_api::{DVec, Direction};

use crate::{JobMode, op::Operation};

pub mod cna;
mod def;
pub mod dpu;
pub mod op;

#[derive(Debug, Clone)]
pub struct SubmitBase {
    pub flags: JobMode,
    pub task_base_addr: u32,
    pub core_idx: usize,
    pub int_mask: u32,
    pub int_clear: u32,
    pub regcfg_amount: u32,
}

#[derive(Debug, Clone)]
pub struct SubmitRef {
    pub base: SubmitBase,
    pub task_number: usize,
    pub regcmd_base_addr: u32,
}

pub struct Submit {
    pub base: SubmitBase,
    pub regcmd_all: DVec<u64>,
    pub tasks: Vec<Operation>,
}

impl Submit {
    pub fn new(tasks: Vec<Operation>) -> Self {
        let base = SubmitBase {
            flags: JobMode::PC | JobMode::BLOCK | JobMode::PINGPONG,
            task_base_addr: 0,
            core_idx: 0,
            int_mask: 0x300, // wait for DPU to finish
            int_clear: 0x1ffff,
            regcfg_amount: tasks[0].reg_amount(),
        };

        let regcmd_all: DVec<u64> = DVec::zeros(
            u32::MAX as _,
            base.regcfg_amount as usize * tasks.len(),
            0x1000,
            Direction::Bidirectional,
        )
        .unwrap();

        assert!(
            regcmd_all.bus_addr() <= u32::MAX as u64,
            "regcmd base address exceeds u32"
        );

        let amount = base.regcfg_amount as usize;
        for (i, task) in tasks.iter().enumerate() {
            let regcmd = unsafe {
                core::slice::from_raw_parts_mut(regcmd_all.as_ptr().add(i * amount), amount)
            };
            task.fill_regcmd(regcmd);
        }
        regcmd_all.confirm_write_all();

        Self {
            base,
            regcmd_all,
            tasks,
        }
    }

    pub fn as_ref(&self) -> SubmitRef {
        SubmitRef {
            base: self.base.clone(),
            task_number: self.tasks.len(),
            regcmd_base_addr: self.regcmd_all.bus_addr() as _,
        }
    }
}
