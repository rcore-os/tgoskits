use axvm::GuestPhysAddr;

pub const GUEST_PHYS_MEMORY_BASE: GuestPhysAddr = 0x5000_0000;
pub const BIOS_ENTRY: GuestPhysAddr = 0x8000;
pub const DTB_ENTRY: GuestPhysAddr = 0x0;
pub const GUEST_ENTRY: GuestPhysAddr = 0x5008_0000;
pub const GUEST_PHYS_MEMORY_SIZE: usize = 0x100_0000; // 16M
