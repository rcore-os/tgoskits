#[allow(dead_code)]
pub mod irq {
    pub const SWI0: u32 = 0;
    pub const SWI1: u32 = 1;
    pub const HWI0: u32 = 2;
    pub const HWI1: u32 = 3;
    pub const HWI2: u32 = 4;
    pub const HWI3: u32 = 5;
    pub const HWI4: u32 = 6;
    pub const HWI5: u32 = 7;
    pub const HWI6: u32 = 8;
    pub const HWI7: u32 = 9;
    pub const PCOV: u32 = 10;
    pub const TI: u32 = 11;
    pub const IPI: u32 = 12;
    pub const NMI: u32 = 13;
    pub const AVEC: u32 = 14;
}

pub mod csr {
    pub const PRMD: usize = 0x1;
    pub const ERA: usize = 0x6;
    /// Bad Virtual Address - 触发地址相关异常的虚拟地址
    pub const BADV: usize = 0x7;
}
