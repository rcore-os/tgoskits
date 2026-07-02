mod writer;

pub use writer::FdtWriter;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub use writer::FdtWriterNode;

/// Magic number used in the FDT header.
pub const FDT_MAGIC: u32 = 0xd00dfeed;

pub const FDT_BEGIN_NODE: u32 = 0x00000001;
pub const FDT_END_NODE: u32 = 0x00000002;
pub const FDT_PROP: u32 = 0x00000003;
pub const FDT_END: u32 = 0x00000009;

pub const NODE_NAME_MAX_LEN: usize = 31;
pub const PROPERTY_NAME_MAX_LEN: usize = 63;
