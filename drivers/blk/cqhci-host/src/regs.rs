//! CQHCI register and descriptor definitions.

pub const VER: usize = 0x00;
pub const CAP: usize = 0x04;
pub const CFG: usize = 0x08;
pub const CTL: usize = 0x0c;
pub const IS: usize = 0x10;
pub const ISTE: usize = 0x14;
pub const ISGE: usize = 0x18;
pub const TDLBA: usize = 0x20;
pub const TDLBAU: usize = 0x24;
pub const TDBR: usize = 0x28;
pub const TCN: usize = 0x2c;
pub const TDPE: usize = 0x3c;
pub const SSC2: usize = 0x44;
pub const CRDCT: usize = 0x48;
pub const TERRI: usize = 0x54;

pub const CFG_DCMD: u32 = 1 << 12;
pub const CFG_TASK_DESC_128: u32 = 1 << 8;
pub const CFG_CRYPTO_GENERAL_ENABLE: u32 = 1 << 1;
pub const CFG_ENABLE: u32 = 1;

pub const CTL_CLEAR_ALL_TASKS: u32 = 1 << 8;
pub const CTL_HALT: u32 = 1;

pub const IS_TASK_COMPLETE: u32 = 1 << 1;
pub const IS_RESPONSE_ERROR: u32 = 1 << 2;
pub const IS_GENERAL_CRYPTO_ERROR: u32 = 1 << 4;
pub const IS_INVALID_CRYPTO_CONFIG: u32 = 1 << 5;
pub const IS_ERROR_MASK: u32 =
    IS_RESPONSE_ERROR | IS_GENERAL_CRYPTO_ERROR | IS_INVALID_CRYPTO_CONFIG;
pub const IS_RUNTIME_MASK: u32 = IS_TASK_COMPLETE | IS_ERROR_MASK;

pub const ATTR_VALID: u64 = 1;
pub const ATTR_END: u64 = 1 << 1;
pub const ATTR_INTERRUPT: u64 = 1 << 2;
pub const ATTR_ACT_TASK: u64 = 0x5 << 3;
pub const ATTR_ACT_TRANSFER: u64 = 0x4 << 3;
pub const ATTR_ACT_LINK: u64 = 0x6 << 3;
pub const TASK_DATA_READ: u64 = 1 << 12;
pub const TASK_QUEUE_BARRIER: u64 = 1 << 14;
pub const TASK_BLOCK_COUNT_SHIFT: u32 = 16;
pub const TASK_BLOCK_ADDRESS_SHIFT: u32 = 32;
pub const DCMD_COMMAND_SHIFT: u32 = 16;
pub const DCMD_TIMING_SHIFT: u32 = 22;
pub const DCMD_RESPONSE_SHIFT: u32 = 23;
pub const TRANSFER_LENGTH_SHIFT: u32 = 16;
pub const DESCRIPTOR_ADDRESS_SHIFT: u32 = 32;

pub const DATA_SLOTS: usize = 31;
pub const DIRECT_COMMAND_SLOT: usize = 31;
pub const TOTAL_SLOTS: usize = 32;
pub const TRANSFER_DESCRIPTORS_PER_SLOT: usize = 128;
pub const MAX_TRANSFER_DESCRIPTOR_BYTES: usize = 1 << 16;
