/// The size of the kernel stack.
pub const KERNEL_STACK_SIZE: usize = 0x4_0000;

/// The base address of the user space.
pub const USER_SPACE_BASE: usize = 0x1000;
/// The size of the user space.
///
/// 128 TiB, matching aarch64/x86_64 (#242). LoongArch LA64 uses a 4-level page
/// table with a 48-bit VA (`page_table_multiarch` `LA64MetaData`: LEVELS=4,
/// VA_MAX_BITS=48), so the low-half user window widens to the same
/// `0x7fff_ffff_f000` as aarch64. The previous 256 GiB window predated the #242
/// widen and was too small for high virtual reservations such as the JVM
/// CompressedOops heap base (HotSpot probes 2 GiB → 4/32 GiB), which on loong
/// landed above the old `0x40_0000_0000` top and was rejected by
/// `AddrSpace::validate_region` (`NoMemory: address out of range`), hanging the
/// multi-JDK language carpet. (riscv64 stays 256 GiB — it is Sv39, 39-bit VA.)
pub const USER_SPACE_SIZE: usize = 0x7fff_ffff_f000;

/// The highest address of the user stack.
///
/// Placed at 4 TiB (mirroring aarch64/x86_64, #242) so ~124 TiB of VA remains
/// above the stack for large virtual reservations (JVM CompressedOops heap, Go
/// arenas). The previous `0x4_0000_0000` (16 GiB) left no headroom above the
/// stack within the old window.
pub const USER_STACK_TOP: usize = 0x0400_0000_0000;
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
