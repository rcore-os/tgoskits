include!(concat!(env!("OUT_DIR"), "/defines.rs"));

pub const KERNEL_BASE: usize = VM_LOAD_ADDRESS;
pub const PERCPU_BASE: usize = 0xffff_ff00_0000_0000;
pub const KERNEL_SPACE_BASE: usize = 0xffff_8000_0000_0000;
