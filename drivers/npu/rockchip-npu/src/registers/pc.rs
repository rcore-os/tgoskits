use tock_registers::{interfaces::*, register_structs, registers::*};

use crate::registers::consts::INT_CLEAR_ALL;

register_structs! {
    #[allow(non_snake_case)]
    pub PcRegs {
        (0x0000 => pub version: ReadOnly<u32>),
        (0x0004 => pub version_num: ReadOnly<u32>),
        (0x0008 => pub operation_enable: ReadWrite<u32, PC_OPERATION_ENABLE::Register>),
        (0x000C => _reserved0),
        (0x0010 => pub base_address: ReadWrite<u32, PC_BASE_ADDRESS::Register>),
        (0x0014 => pub register_amounts: ReadWrite<u32, PC_REGISTER_AMOUNTS::Register>),
        (0x0018 => _reserved1),
        (0x0020 => pub interrupt_mask: ReadWrite<u32, PC_INTERRUPT::Register>),
        (0x0024 => pub interrupt_clear: WriteOnly<u32, PC_INTERRUPT::Register>),
        (0x0028 => pub interrupt_status: ReadOnly<u32, PC_INTERRUPT::Register>),
        (0x002C => pub interrupt_raw_status: ReadOnly<u32, PC_INTERRUPT::Register>),
        (0x0030 => pub task_control: ReadWrite<u32, PC_TASK_CONTROL::Register>),
        (0x0034 => pub task_dma_base_addr: ReadWrite<u32, PC_TASK_DMA_BASE::Register>),
        (0x0038 => _reserved2),
        (0x003C => pub task_status: ReadOnly<u32>),
        (0x0040 => @END),
    }
}

tock_registers::register_bitfields! {u32,
    PC_OPERATION_ENABLE [
        OP_EN OFFSET(0) NUMBITS(1) []
    ],

    PC_BASE_ADDRESS [
        SOURCE_ADDR OFFSET(0) NUMBITS(32) []
    ],

    PC_REGISTER_AMOUNTS [
        AMOUNT OFFSET(0) NUMBITS(16) [],
        RESERVED OFFSET(16) NUMBITS(16) []
    ],

    PC_INTERRUPT [
        CNA_FG0 OFFSET(0) NUMBITS(1) [],
        CNA_FG1 OFFSET(1) NUMBITS(1) [],
        CNA_WG0 OFFSET(2) NUMBITS(1) [],
        CNA_WG1 OFFSET(3) NUMBITS(1) [],
        CNA_CSC0 OFFSET(4) NUMBITS(1) [],
        CNA_CSC1 OFFSET(5) NUMBITS(1) [],
        CORE_G0 OFFSET(6) NUMBITS(1) [],
        CORE_G1 OFFSET(7) NUMBITS(1) [],
        DPU_G0 OFFSET(8) NUMBITS(1) [],
        DPU_G1 OFFSET(9) NUMBITS(1) [],
        PPU_G0 OFFSET(10) NUMBITS(1) [],
        PPU_G1 OFFSET(11) NUMBITS(1) [],
        DMA_RD_ERR OFFSET(12) NUMBITS(1) [],
        DMA_WR_ERR OFFSET(13) NUMBITS(1) []
    ],

    PC_TASK_CONTROL [
        TASK_NUMBER OFFSET(0) NUMBITS(12) [],
        TASK_PP_EN OFFSET(12) NUMBITS(1) [],
        TASK_COUNT_CLEAR OFFSET(13) NUMBITS(1) [],
        RESERVED OFFSET(14) NUMBITS(18) []
    ],

    PC_TASK_DMA_BASE [
        BASE_ADDR OFFSET(4) NUMBITS(28) []
    ]
}

impl PcRegs {
    pub fn version(&self) -> u32 {
        self.version
            .get()
            .wrapping_add(self.version_num.get() & 0xffff)
    }

    pub fn clean_interrupts(&self) {
        self.interrupt_clear.set(INT_CLEAR_ALL);
    }
}
