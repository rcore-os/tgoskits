/// The size of the kernel stack.
pub const KERNEL_STACK_SIZE: usize = 0x4_0000;

/// The base address of the user space.
pub const USER_SPACE_BASE: usize = 0x1000;
/// Maximum Starry ABI size of the user space.
///
/// The runtime MM layout intersects this 128-TiB policy ceiling with the
/// CPUCFG-derived canonical lower half. A VALEN=40 CPU therefore receives the
/// Linux-equivalent 512-GiB TASK_SIZE without a board-specific feature.
pub const USER_SPACE_MAX_SIZE: usize = 0x7fff_ffff_f000;

/// The highest address of the user stack.
///
/// Placed at 4 TiB (mirroring aarch64/x86_64, #242) so ~124 TiB of VA remains
/// above the stack for large virtual reservations (JVM CompressedOops heap, Go
/// arenas). Runtime layout clips this ceiling to TASK_SIZE on CPUs whose VALEN
/// cannot represent 4 TiB.
pub const USER_STACK_TOP_MAX: usize = 0x0400_0000_0000;
/// The size of the user stack.
pub const USER_STACK_SIZE: usize = 0x80_0000;

/// The lowest address of the user heap.
pub const USER_HEAP_BASE: usize = 0x4000_0000;
/// The size of the user heap.
pub const USER_HEAP_SIZE: usize = 0x1_0000;  // 64KB
/// The maximum size of the user heap (for brk expansion).
pub const USER_HEAP_SIZE_MAX: usize = 0x2000_0000;  // 512MB

/// The address of signal trampoline (placed at top of user heap).
pub const SIGNAL_TRAMPOLINE: usize = 0x6000_1000;
