/// The size of the kernel stack.
pub const KERNEL_STACK_SIZE: usize = 0x4_0000;

/// The base address of the user space.
pub const USER_SPACE_BASE: usize = 0x1000;
/// The size of the user space.
pub const USER_SPACE_SIZE: usize = 0x7fff_ffff_f000;

/// The highest address of the user stack.
pub const USER_STACK_TOP: usize = 0x7fff_0000_0000;
/// The size of the user stack.
pub const USER_STACK_SIZE: usize = 0x8_0000;

/// The lowest address of the user heap.
pub const USER_HEAP_BASE: usize = 0x4000_0000;
/// The size of the user heap.
pub const USER_HEAP_SIZE: usize = 0x1_0000;

/// The base address for user interpreter.
pub const USER_INTERP_BASE: usize = 0x400_0000;

/// The address of signal trampoline.
pub const SIGNAL_TRAMPOLINE: usize = 0x4001_0000;
