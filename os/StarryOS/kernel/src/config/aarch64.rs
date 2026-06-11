/// The size of the kernel stack.
pub const KERNEL_STACK_SIZE: usize = 0x4_0000;

/// The base address of the user space.
pub const USER_SPACE_BASE: usize = 0x1000;
/// The size of the user space.
pub const USER_SPACE_SIZE: usize = 0x7fff_ffff_f000;

/// The highest address of the user stack.
///
/// See x86_64.rs rationale (#242). aarch64 shares the 128 TiB VA layout and
/// the same 4 GiB-above-stack squeeze that crashed V8 pointer-compression.
pub const USER_STACK_TOP: usize = 0x0400_0000_0000;
/// The size of the user stack.
pub const USER_STACK_SIZE: usize = 0x80_0000;

/// The lowest address of the user heap.
pub const USER_HEAP_BASE: usize = 0x4000_0000;
/// The size of the user heap.
pub const USER_HEAP_SIZE: usize = 0x1_0000;
/// The maximum size of the user heap (for brk expansion).
pub const USER_HEAP_SIZE_MAX: usize = 0x2000_0000;

/// The address of signal trampoline (placed at top of user heap).
pub const SIGNAL_TRAMPOLINE: usize = 0x6000_1000;
