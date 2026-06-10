/// The size of the kernel stack.
pub const KERNEL_STACK_SIZE: usize = 0x4_0000;

/// The base address of the user space.
pub const USER_SPACE_BASE: usize = 0x1000;
/// The size of the user space.
pub const USER_SPACE_SIZE: usize = 0x7fff_ffff_f000;

/// The highest address of the user stack.
///
/// Placed at 4 TiB so that ~124 TiB of VA remains above the stack for large
/// virtual reservations such as V8's 4 GiB pointer-compression cage, JVM
/// CompressedOops heap, and Go runtime arenas. Linux x86_64 keeps the stack
/// near 128 TiB but with VM_GROWSDOWN + huge mmap_base gap; StarryOS lacks
/// growsdown and had only one 4 GiB slot above the previous
/// `0x7fff_0000_0000` stack top, which forced V8 (#242) into a single hint
/// with no fallback and crashed npm/vue/astro with rc=139.
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
