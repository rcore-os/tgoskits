//! Per-CPU cache leaves behind `/sys/devices/system/cpu/cpuN/cache/`.
//!
//! Only architectures with cache-geometry registers build this module. RISC-V
//! describes caches solely in the device tree, so its CPUs expose no `cache/`.

use alloc::{borrow::Cow, boxed::Box, format, string::String, sync::Arc, vec::Vec};

use ax_lazyinit::OnceLock;
use ax_runtime::hal::{
    cpu_num,
    irq::{CpuId, run_on_cpu_sync},
    percpu::this_cpu_id,
};
use axfs_ng_vfs::{VfsError, VfsResult};

use super::{cpu_bit_mask, cpu_hex_mask, cpu_range_string};
use crate::{
    pseudofs::{NodeOpsMux, SimpleDir, SimpleDirOps, SimpleFile, SimpleFs},
    sync::IrqMutex,
};

/// Room for every enumeration below: x86 leaf 4 stops here, leaf 0x2 yields at
/// most four leaves, arm64 walks seven levels and loongarch64 three, each with at
/// most a data and an instruction leaf.
const MAX_CACHE_LEAVES: usize = 16;

/// One CPU's leaves in a fixed-capacity buffer, so they can be read in hard-IRQ
/// context on that CPU without allocating.
type LocalLeaves = heapless::Vec<CacheLeaf, MAX_CACHE_LEAVES>;

/// Cache type as reported by the arch's cache-detection facility. Mirrors Linux
/// `enum cache_type` (`include/linux/cacheinfo.h`) and its `type_show()` strings.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CacheType {
    Data,
    Instruction,
    Unified,
}

impl CacheType {
    fn as_str(self) -> &'static str {
        match self {
            CacheType::Data => "Data",
            CacheType::Instruction => "Instruction",
            CacheType::Unified => "Unified",
        }
    }
}

/// Which CPUs share a given cache leaf, mirroring how Linux builds
/// `cacheinfo.shared_cpu_map`.
///
/// Linux fills `shared_cpu_map` on two distinct paths:
///
/// - **aarch64 / loongarch64** enumerate from architecture registers with no
///   thread-sharing count, i.e. the `use_arch_info` path in
///   `drivers/base/cacheinfo.c`. There `cache_leaves_are_shared()`
///   (cacheinfo.c:47-49) reduces to `this_leaf->level != 1 && sib_leaf->level
///   != 1`: L1 is private per CPU, every higher level is system-wide shared.
///
/// - **x86** overrides that with `__cache_cpumap_setup()`
///   (arch/x86/kernel/cpu/cacheinfo.c:546-580), which uses the per-leaf
///   `num_threads_sharing` count from `CPUID.4` to build a bounded APIC domain
///   `apicid >> order_base_2(num_threads_sharing)`. Under the single-socket,
///   no-SMT, no-sub-package-cluster topology this sysfs tree models (every
///   `cpuN/topology/` reports `physical_package_id 0`, `core_id == cpu`, and a
///   `package_cpus` mask of all online CPUs), a consistent leaf is per-core when
///   `num_threads_sharing == 1` (private) or package-wide when shared by every
///   online CPU (the same set `package_cpus` / `core_siblings` render). A leaf
///   whose count falls strictly between - shared by a subset larger than one -
///   would need the exact APIC domain this flat tree cannot reconstruct and also
///   contradicts the no-SMT/single-package geometry the rest of the tree
///   presents; it is recorded as [`Unknown`] so the ambiguous `shared_cpu_map` /
///   `shared_cpu_list` are omitted rather than fabricated (neither owner-only nor
///   whole-package). The homogeneous `-smp 4` QEMU models never produce it.
///
/// `SharingScope` records the per-leaf decision so `shared_cpu_map` /
/// `shared_cpu_list` can be rebuilt for any logical CPU.
///
/// [`Unknown`]: SharingScope::Unknown
#[derive(Clone, Copy, PartialEq, Eq)]
enum SharingScope {
    /// The owning CPU only (Linux's L1-private rule on the arch-info path, and
    /// x86 leaves whose `num_threads_sharing == 1`). With no SMT modelled the
    /// owner has no thread siblings, so this is exactly `{cpu}`.
    Private,
    /// Every online CPU: the package domain. This is Linux's "system-wide
    /// shared" rule for L2+ caches on the `use_arch_info` path (aarch64 /
    /// loongarch64, which carry no thread-sharing count), and equally the
    /// package-wide domain any x86 leaf shared beyond its owner collapses to
    /// under this single-socket topology (see the type doc).
    SystemWide,
    /// A cache shared beyond its owner but by a strict subset of the online CPUs
    /// (an x86 leaf with `1 < num_threads_sharing < online`). The exact members
    /// need the per-CPU APIC-id grouping `__cache_cpumap_setup()` performs
    /// (`apicid >> order_base_2(num_threads_sharing)`), which this flat-topology
    /// tree cannot reconstruct. Such a leaf also contradicts the flat topology
    /// the rest of the tree presents (no SMT, single package), so rather than
    /// widen the set to every CPU (over-reporting a partial cache as package-wide)
    /// or shrink it to the owner (under-reporting), `shared_cpu_map` /
    /// `shared_cpu_list` are *omitted*, exactly as Linux omits any cache attribute
    /// it cannot back with a real value. Never produced by the register-only path
    /// or by the homogeneous `-smp 4` QEMU models (where a leaf is shared by 1 or
    /// by all CPUs), so it is guarded by the `arch_info_scope` unit test.
    Unknown,
}

/// One cache leaf as enumerated from firmware/architecture registers, mirroring Linux's
/// `struct cacheinfo`. Geometry fields are `Option` so that any attribute the hardware does
/// not report is *omitted* from sysfs rather than fabricated - exactly like Linux's
/// `cache_default_attrs_is_visible()`, which hides an attribute whose backing value is 0.
#[derive(Clone, Copy)]
struct CacheLeaf {
    level: u32,
    ctype: CacheType,
    /// Total cache size in bytes.
    size: Option<u32>,
    /// `coherency_line_size` in bytes.
    line_size: Option<u32>,
    /// `number_of_sets`.
    sets: Option<u32>,
    /// `ways_of_associativity`.
    ways: Option<u32>,
    /// `physical_line_partition` (lines per tag).
    partition: Option<u32>,
    /// Which CPUs share this leaf, per Linux `cache_leaves_are_shared()` on the
    /// arch-info path (see [`SharingScope`]).
    sharing: SharingScope,
}

impl CacheLeaf {
    /// Assign the sharing scope for this leaf.
    ///
    /// `threads_sharing` is `None` on the register-only `use_arch_info` path
    /// (aarch64 / loongarch64), where Linux's `cache_leaves_are_shared()`
    /// (cacheinfo.c:47-49) gives L1 private and every higher level system-wide.
    ///
    /// x86 passes `Some(num_threads_sharing)` from `CPUID.4`, mapped against the
    /// online CPU count (the package size this flat tree models) by
    /// [`arch_info_scope`]:
    /// - `num_threads_sharing == 1` (or L1) -> the owning CPU only, matching the
    ///   early return in `__cache_cpumap_setup()` (cacheinfo.c:564).
    /// - shared by every online CPU -> the whole package (every online CPU, the
    ///   mask `package_cpus` / `core_siblings` also render), so [`SystemWide`].
    /// - shared by a strict subset (`1 < n < online`) -> [`Unknown`], whose exact
    ///   members `__cache_cpumap_setup()` derives from per-CPU APIC ids this tree
    ///   does not track; the attribute is omitted rather than fabricated (neither
    ///   owner-only under-reporting nor whole-package over-reporting).
    ///
    /// [`SystemWide`]: SharingScope::SystemWide
    /// [`Unknown`]: SharingScope::Unknown
    fn with_arch_info_sharing(mut self, threads_sharing: Option<u32>) -> Self {
        self.sharing = arch_info_scope(self.level, threads_sharing, cpu_num() as u32);
        self
    }
}

/// Sharing scope for a leaf sampled on the register-only `use_arch_info` path,
/// from its `level` and, on x86, `CPUID.4` `num_threads_sharing`. Split out from
/// [`CacheLeaf::with_arch_info_sharing`] so it can be unit-tested without a live
/// CPU: the bug it guards (a cache shared beyond its owner reported owner-only)
/// only manifests on a topology with intermediate cache sharing, which the
/// homogeneous `-smp 4` QEMU used by the on-target test never produces.
fn arch_info_scope(level: u32, threads_sharing: Option<u32>, online_cpus: u32) -> SharingScope {
    match (level, threads_sharing) {
        // L1, or an x86 leaf CPUID.4 reports as used by a single thread: private.
        (1, _) | (_, Some(1)) => SharingScope::Private,
        // x86 leaf shared by (at least) every online CPU: provably the whole
        // package. `>=` covers a count rounded up past the online set.
        (_, Some(n)) if n >= online_cpus => SharingScope::SystemWide,
        // x86 leaf shared by a strict subset (`1 < n < online`): the exact members
        // need APIC grouping this flat model lacks, so omit rather than fabricate
        // an all-CPU set (see [`SharingScope::Unknown`]).
        (_, Some(_)) => SharingScope::Unknown,
        // Register-only path (aarch64 / loongarch64, no count): L1 private (handled
        // above), every higher level system-wide - Linux's `cache_leaves_are_shared()`.
        (_, None) => SharingScope::SystemWide,
    }
}

/// Enumerate the **executing PE's** real cache hierarchy from architecture registers.
///
/// The architecture cache registers can only describe the core currently
/// running the instruction (CPUID / CCSIDR / CPUCFG are all per-PE), so this
/// must run on a real CPU, not a later `cpuN/cache` read that may land on any
/// PE. It is called once at boot from [`init_cpu_cache`] on the primary CPU and
/// the result is served for every logical CPU (see [`cpu_cache_leaves`]).
///
/// - x86_64: `CPUID` leaf 4 (deterministic cache parameters), decoded per
///   `arch/x86/kernel/cpu/cacheinfo.c` (`cpuid4_info_fill_done`).
/// - aarch64: `CLIDR_EL1` for present levels/types (`arch/arm64/kernel/cacheinfo.c`
///   `get_cache_type`) and `CCSIDR_EL1` (selected via `CSSELR_EL1`) for the geometry, the
///   ARM-architected cache-size register.
/// - loongarch64: `CPUCFG` leaves 16/17.. (`arch/loongarch/mm/cache.c` `cpu_cache_init`).
/// - riscv64: RISC-V defines no cache-geometry registers; Linux relies solely on the device
///   tree (`arch/riscv/kernel/cacheinfo.c` `init_of_cache_level`). With no DT cacheinfo
///   parser here, no leaf can be produced without fabricating, so the list is empty and the
///   `cache/` directory is not exposed - the same "unavailable => absent" outcome Linux
///   gives when firmware carries no cacheinfo.
fn read_local_cache_leaves() -> LocalLeaves {
    #[cfg(target_arch = "x86_64")]
    {
        read_cache_leaves_x86()
    }
    #[cfg(target_arch = "aarch64")]
    {
        read_cache_leaves_aarch64()
    }
    #[cfg(target_arch = "loongarch64")]
    {
        read_cache_leaves_loongarch64()
    }
}

/// Per-CPU cache leaves, each sampled *on its own logical CPU* during boot.
///
/// This mirrors Linux's `per_cpu(ci_cpu_cacheinfo, cpu)`
/// (`drivers/base/cacheinfo.c:27`), which detects each CPU's cache in the
/// `cacheinfo_cpu_online` hotplug callback so a heterogeneous (big.LITTLE)
/// machine reports the true geometry of every cluster. [`init_cpu_cache`] drives
/// the equivalent: it runs [`read_local_cache_leaves`] on each online CPU - the
/// primary directly, every secondary through an IPI - so `slot[cpu]` holds the
/// cache registers of `cpu` itself, not a copy of the primary's.
///
/// `OnceLock` guards the one-time table allocation. Each slot is an IRQ-safe
/// [`IrqMutex`] because it is written by [`init_cpu_cache`] once the sampling call
/// returns and read later from `cpuN/cache` sysfs reads; the `Vec<CacheLeaf>` is heap-backed
/// (the allocator is up by the time sysfs mounts).
static PER_CPU_CACHE_LEAVES: OnceLock<Vec<IrqMutex<Vec<CacheLeaf>>>> = OnceLock::new();

/// Detect every online CPU's cache leaves, each on that CPU.
///
/// The cache registers (`CPUID` / `CCSIDR` / `CPUCFG`) only describe the
/// executing core, so each CPU runs [`sample_local_cache_leaves`] itself through
/// the synchronous cross-CPU call, which returns after that CPU filled the
/// caller's buffer.
/// Mirrors Linux running `detect_cache_attributes(cpu)` on `cpu` at its online
/// callback. Called from `mount_all` on the primary once all secondaries are up,
/// before sysfs is mounted.
pub fn init_cpu_cache() {
    let total = cpu_num();
    PER_CPU_CACHE_LEAVES.call_once(|| (0..total).map(|_| IrqMutex::new(Vec::new())).collect());
    let Some(table) = PER_CPU_CACHE_LEAVES.get() else {
        return;
    };

    for (cpu, slot) in table.iter().enumerate() {
        let mut leaves = LocalLeaves::new();
        // SAFETY: `leaves` outlives the synchronous call and nothing else touches
        // it until the call returns.
        let sampled = unsafe {
            run_on_cpu_sync(CpuId(cpu), sample_local_cache_leaves, (&raw mut leaves).cast())
        };
        // A CPU the call cannot reach keeps its empty slot, so `cpu_cache_leaves`
        // reports no leaves for it rather than another CPU's registers.
        if sampled.is_ok() {
            *slot.lock() = leaves.to_vec();
        }
    }
}

/// Fills the caller's buffer with the executing CPU's cache leaves.
///
/// It runs in hard-IRQ context on the sampled CPU, where the cross-CPU call
/// forbids allocating, dropping owned state or taking a lock the caller may hold,
/// so the leaves land in a fixed-capacity buffer that [`init_cpu_cache`] publishes
/// once the call returns.
///
/// # Safety
///
/// `arg` must point to a live `LocalLeaves` that nothing else accesses until the
/// call returns.
unsafe fn sample_local_cache_leaves(arg: *mut ()) {
    // SAFETY: guaranteed by the caller.
    unsafe { *arg.cast::<LocalLeaves>() = read_local_cache_leaves() };
}

/// The cache leaves reported for logical CPU `cpu`, sampled on that CPU.
///
/// Returns the set [`init_cpu_cache`] read on `cpu` itself (see
/// [`PER_CPU_CACHE_LEAVES`]), so heterogeneous cores report their own geometry
/// rather than the primary's.
///
/// Invariant: `/sys/.../cpuN/cache` must reflect `cpuN`'s own registers no
/// matter which PE services the read. The architecture cache registers describe
/// only the executing core, so a live [`read_local_cache_leaves`] is valid for
/// `cpu` *only* when it runs on `cpu`. Once the per-CPU table is built, that
/// pinned slot is the single source of truth; a slot left empty by a failed
/// sampling call yields an empty result (so `cpuN/cache` is absent) rather than
/// being backfilled from the reader's PE, which would return another CPU's data.
/// A live read is used only for the pre-`init_cpu_cache` boot race, and only
/// when `cpu == this_cpu_id()` so it still describes `cpu`.
fn cpu_cache_leaves(cpu: usize) -> Vec<CacheLeaf> {
    match PER_CPU_CACHE_LEAVES.get() {
        Some(table) => table
            .get(cpu)
            .map(|slot| slot.lock().clone())
            .unwrap_or_default(),
        None if cpu == this_cpu_id() => read_local_cache_leaves().to_vec(),
        None => Vec::new(),
    }
}

pub(super) fn has_cache_leaves(cpu: usize) -> bool {
    !cpu_cache_leaves(cpu).is_empty()
}

/// x86_64 cache enumeration, mirroring `arch/x86/kernel/cpu/cacheinfo.c`:
/// prefer `CPUID` leaf 4 (deterministic cache parameters); when it reports no
/// caches, fall back to the legacy `CPUID` leaf 0x2 descriptor table
/// (`intel_cacheinfo_0x2`), exactly as Linux does when leaf 4 is unavailable.
#[cfg(target_arch = "x86_64")]
fn read_cache_leaves_x86() -> LocalLeaves {
    let leaves = read_cache_leaves_x86_leaf4();
    if !leaves.is_empty() {
        return leaves;
    }
    read_cache_leaves_x86_leaf2()
}

/// `CPUID` leaf 4: iterate subleaves until a NULL cache type, decoding EAX/EBX/ECX per
/// Intel SDM / `arch/x86/kernel/cpu/cacheinfo.c`. `size = (sets+1)*(line+1)*(part+1)*(ways+1)`.
#[cfg(target_arch = "x86_64")]
fn read_cache_leaves_x86_leaf4() -> LocalLeaves {
    let mut leaves = LocalLeaves::new();
    for subleaf in 0..u32::MAX {
        let r = core::arch::x86_64::__cpuid_count(4, subleaf);
        let cache_type = r.eax & 0x1f;
        let ctype = match cache_type {
            1 => CacheType::Data,
            2 => CacheType::Instruction,
            3 => CacheType::Unified,
            _ => break, // 0 = NULL: no more caches.
        };
        let level = (r.eax >> 5) & 0x7;
        // EAX bits 25:14 hold `num_threads_sharing - 1` (Intel SDM leaf 4 /
        // `union _cpuid4_leaf_eax.num_threads_sharing`); +1 gives the count of
        // logical processors that share this cache, which `get_cache_id()` /
        // `__cache_cpumap_setup()` use to build shared_cpu_map.
        let threads_sharing = ((r.eax >> 14) & 0xfff) + 1;
        let line = (r.ebx & 0xfff) + 1;
        let partition = ((r.ebx >> 12) & 0x3ff) + 1;
        let ways = ((r.ebx >> 22) & 0x3ff) + 1;
        let sets = r.ecx + 1;
        let size = sets * line * partition * ways;
        let _ = leaves.push(
            CacheLeaf {
                level,
                ctype,
                size: Some(size),
                line_size: Some(line),
                sets: Some(sets),
                ways: Some(ways),
                partition: Some(partition),
                sharing: SharingScope::Private,
            }
            .with_arch_info_sharing(Some(threads_sharing)),
        );
        if leaves.is_full() {
            break;
        }
    }
    leaves
}

/// A `CPUID` leaf 0x2 one-byte cache descriptor: which cache it describes and
/// its total size in bytes. Mirrors the cache rows of Linux's
/// `cpuid_0x2_table[256]` (`arch/x86/kernel/cpu/cpuid_0x2_table.c`); the TLB
/// descriptor rows are intentionally omitted (they carry no cache geometry).
#[cfg(target_arch = "x86_64")]
struct Leaf2Cache {
    level: u32,
    ctype: CacheType,
    /// Total cache size in bytes.
    size: u32,
    /// `coherency_line_size` in bytes, per the Intel SDM descriptor definition.
    line: u32,
}

/// Map a leaf 0x2 descriptor byte to its cache geometry, transcribed 1:1 from
/// the `CACHE_ENTRY(...)` rows of Linux's `cpuid_0x2_table[]`. `size` is in
/// bytes (Linux stores KiB via `/ SZ_1K`; we keep bytes and match [`CacheLeaf`]).
/// `line` is the coherency line size named in each table row's comment (the
/// Intel SDM descriptor definition), which Linux does not store but sysfs
/// exposes when known.
#[cfg(target_arch = "x86_64")]
fn leaf2_cache_descriptor(desc: u8) -> Option<Leaf2Cache> {
    use CacheType::{Data, Instruction, Unified};
    const K: u32 = 1024;
    const M: u32 = 1024 * 1024;
    let (level, ctype, size, line) = match desc {
        0x06 => (1, Instruction, 8 * K, 32),
        0x08 => (1, Instruction, 16 * K, 32),
        0x09 => (1, Instruction, 32 * K, 64),
        0x0a => (1, Data, 8 * K, 32),
        0x0c => (1, Data, 16 * K, 32),
        0x0d => (1, Data, 16 * K, 64),
        0x0e => (1, Data, 24 * K, 64),
        0x21 => (2, Unified, 256 * K, 64),
        0x22 => (3, Unified, 512 * K, 64),
        0x23 => (3, Unified, M, 64),
        0x25 => (3, Unified, 2 * M, 64),
        0x29 => (3, Unified, 4 * M, 64),
        0x2c => (1, Data, 32 * K, 64),
        0x30 => (1, Instruction, 32 * K, 64),
        0x39 => (2, Unified, 128 * K, 64),
        0x3a => (2, Unified, 192 * K, 64),
        0x3b => (2, Unified, 128 * K, 64),
        0x3c => (2, Unified, 256 * K, 64),
        0x3d => (2, Unified, 384 * K, 64),
        0x3e => (2, Unified, 512 * K, 64),
        0x3f => (2, Unified, 256 * K, 64),
        0x41 => (2, Unified, 128 * K, 32),
        0x42 => (2, Unified, 256 * K, 32),
        0x43 => (2, Unified, 512 * K, 32),
        0x44 => (2, Unified, M, 32),
        0x45 => (2, Unified, 2 * M, 32),
        0x46 => (3, Unified, 4 * M, 64),
        0x47 => (3, Unified, 8 * M, 64),
        0x48 => (2, Unified, 3 * M, 64),
        0x49 => (3, Unified, 4 * M, 64),
        0x4a => (3, Unified, 6 * M, 64),
        0x4b => (3, Unified, 8 * M, 64),
        0x4c => (3, Unified, 12 * M, 64),
        0x4d => (3, Unified, 16 * M, 64),
        0x4e => (2, Unified, 6 * M, 64),
        0x60 => (1, Data, 16 * K, 64),
        0x66 => (1, Data, 8 * K, 64),
        0x67 => (1, Data, 16 * K, 64),
        0x68 => (1, Data, 32 * K, 64),
        0x78 => (2, Unified, M, 64),
        0x79 => (2, Unified, 128 * K, 64),
        0x7a => (2, Unified, 256 * K, 64),
        0x7b => (2, Unified, 512 * K, 64),
        0x7c => (2, Unified, M, 64),
        0x7d => (2, Unified, 2 * M, 64),
        0x7f => (2, Unified, 512 * K, 64),
        0x80 => (2, Unified, 512 * K, 64),
        0x82 => (2, Unified, 256 * K, 32),
        0x83 => (2, Unified, 512 * K, 32),
        0x84 => (2, Unified, M, 32),
        0x85 => (2, Unified, 2 * M, 32),
        0x86 => (2, Unified, 512 * K, 64),
        0x87 => (2, Unified, M, 64),
        0xd0 => (3, Unified, 512 * K, 64),
        0xd1 => (3, Unified, M, 64),
        0xd2 => (3, Unified, 2 * M, 64),
        0xd6 => (3, Unified, M, 64),
        0xd7 => (3, Unified, 2 * M, 64),
        0xd8 => (3, Unified, 4 * M, 64),
        0xdc => (3, Unified, 2 * M, 64),
        0xdd => (3, Unified, 4 * M, 64),
        0xde => (3, Unified, 8 * M, 64),
        0xe2 => (3, Unified, 2 * M, 64),
        0xe3 => (3, Unified, 4 * M, 64),
        0xe4 => (3, Unified, 8 * M, 64),
        0xea => (3, Unified, 12 * M, 64),
        0xeb => (3, Unified, 18 * M, 64),
        0xec => (3, Unified, 24 * M, 64),
        _ => return None,
    };
    Some(Leaf2Cache {
        level,
        ctype,
        size,
        line,
    })
}

/// Legacy `CPUID` leaf 0x2 fallback, mirroring `intel_cacheinfo_0x2()`.
///
/// Leaf 0x2 returns four 32-bit registers of one-byte descriptors. AL (the low
/// byte of EAX on the first execution) is the number of times leaf 0x2 must be
/// executed to enumerate every descriptor, per the Intel SDM Vol. 2A CPUID
/// definition - not a validity flag. The leaf is therefore executed AL times and
/// the count byte itself is not a descriptor. A register whose top bit (bit 31)
/// is set carries no valid descriptors and is treated as NULL (`cpuid_leaf_0x2()`,
/// `struct leaf_0x2_reg.invalid`).
///
/// Each remaining descriptor byte is looked up in the cache table; Linux
/// *accumulates* sizes per category (L1I, L1D, L2, L3) and exposes only
/// level/type/size - leaf 0x2 carries no set/way counts, so those attributes are
/// omitted (hidden by `cache_default_attrs_is_visible()`). The coherency line
/// size named in each descriptor is emitted when known.
#[cfg(target_arch = "x86_64")]
fn read_cache_leaves_x86_leaf2() -> LocalLeaves {
    // `CPUID.EAX=2` returns descriptor bytes across EAX/EBX/ECX/EDX.
    let r = core::arch::x86_64::__cpuid(2);

    // Intel requires the iteration count in AL to be 1; otherwise the leaf is
    // not usable and every descriptor is treated as NULL, matching Linux
    // `cpuid_leaf_0x2` (the legacy multi-iteration form is not used).
    if r.eax & 0xff != 0x01 {
        return LocalLeaves::new();
    }

    // Accumulate size (and remember the line size) per category, exactly as
    // `intel_cacheinfo_0x2` sums into l1i/l1d/l2/l3.
    #[derive(Clone, Copy, Default)]
    struct Acc {
        size: u32,
        line: u32,
    }
    let mut l1i = Acc::default();
    let mut l1d = Acc::default();
    let mut l2 = Acc::default();
    let mut l3 = Acc::default();

    for (reg_idx, reg) in [r.eax, r.ebx, r.ecx, r.edx].into_iter().enumerate() {
        // A register with its most-significant bit set holds no valid
        // descriptors (`struct leaf_0x2_reg.invalid`).
        if reg & 0x8000_0000 != 0 {
            continue;
        }
        for byte in 0..4 {
            // Skip the iteration-count byte (low byte of EAX only).
            if reg_idx == 0 && byte == 0 {
                continue;
            }
            let desc = ((reg >> (byte * 8)) & 0xff) as u8;
            if let Some(c) = leaf2_cache_descriptor(desc) {
                let acc = match (c.level, c.ctype) {
                    (1, CacheType::Instruction) => &mut l1i,
                    (1, CacheType::Data) => &mut l1d,
                    (2, _) => &mut l2,
                    _ => &mut l3,
                };
                acc.size += c.size;
                acc.line = c.line;
            }
        }
    }

    // Emit leaves in ascending level order (L1I, L1D, L2, L3), matching the
    // sysfs `index*` ordering; a category with zero accumulated size is absent.
    let mut leaves = LocalLeaves::new();
    let mut push = |acc: Acc, level: u32, ctype: CacheType| {
        if acc.size > 0 {
            // Leaf 0x2 carries no thread-sharing count, so fall back to the
            // level-only arch-info rule (L1 private, L2+ system-wide).
            let _ = leaves.push(
                CacheLeaf {
                    level,
                    ctype,
                    size: Some(acc.size),
                    line_size: (acc.line > 0).then_some(acc.line),
                    sets: None,
                    ways: None,
                    partition: None,
                    sharing: SharingScope::Private,
                }
                .with_arch_info_sharing(None),
            );
        }
    };
    push(l1i, 1, CacheType::Instruction);
    push(l1d, 1, CacheType::Data);
    push(l2, 2, CacheType::Unified);
    push(l3, 3, CacheType::Unified);
    leaves
}

/// aarch64: `CLIDR_EL1` gives the per-level cache type (Ctype: 1=I, 2=D, 3=separate I+D,
/// 4=unified); `CCSIDR_EL1` (after selecting the leaf via `CSSELR_EL1`) gives line
/// size/associativity/sets. Uses the CCIDX-wide `CCSIDR_EL1` layout when
/// `ID_AA64MMFR2_EL1.CCIDX` is set, else the 32-bit layout (ARM ARM D17.2.26).
#[cfg(target_arch = "aarch64")]
fn read_cache_leaves_aarch64() -> LocalLeaves {
    use core::arch::asm;

    // CLIDR_EL1, ID_AA64MMFR2_EL1 and the CSSELR_EL1-select / CCSIDR_EL1-read /
    // CSSELR_EL1-restore sequence must all execute on one PE: CSSELR is a
    // per-PE selection register, so a migration between `msr csselr_el1` and
    // `mrs ccsidr_el1` would read an unselected leaf from another core and
    // restore this core's saved value onto that core. Hold preemption (and
    // therefore migration) off for the whole discovery so every leaf comes from
    // the same PE.
    let _guard = crate::sync::NoPreemptIrqSave::new();

    let clidr: u64;
    let ccidx_field: u64;
    unsafe {
        asm!("mrs {}, clidr_el1", out(reg) clidr, options(nomem, nostack, preserves_flags));
        asm!("mrs {}, id_aa64mmfr2_el1", out(reg) ccidx_field, options(nomem, nostack, preserves_flags));
    }
    let ccidx = ((ccidx_field >> 20) & 0xf) != 0;

    // Read CCSIDR for leaf (level, instruction-side?), restoring CSSELR afterwards.
    let read_ccsidr = |level: u32, instr: bool| -> u64 {
        let sel = ((level as u64 - 1) << 1) | (instr as u64);
        let saved: u64;
        let val: u64;
        unsafe {
            asm!("mrs {}, csselr_el1", out(reg) saved, options(nomem, nostack, preserves_flags));
            asm!("msr csselr_el1, {}", in(reg) sel, options(nomem, nostack, preserves_flags));
            asm!("isb", options(nomem, nostack, preserves_flags));
            asm!("mrs {}, ccsidr_el1", out(reg) val, options(nomem, nostack, preserves_flags));
            asm!("msr csselr_el1, {}", in(reg) saved, options(nomem, nostack, preserves_flags));
            asm!("isb", options(nomem, nostack, preserves_flags));
        }
        val
    };
    let geom = |ccsidr: u64| -> (u32, u32, u32) {
        let (assoc, sets) = if ccidx {
            (
                ((ccsidr >> 3) & 0x1f_ffff) as u32,
                ((ccsidr >> 32) & 0xff_ffff) as u32,
            )
        } else {
            (
                ((ccsidr >> 3) & 0x3ff) as u32,
                ((ccsidr >> 13) & 0x7fff) as u32,
            )
        };
        let line = 1u32 << (((ccsidr & 0x7) as u32) + 4);
        let ways = assoc + 1;
        let nsets = sets + 1;
        (line, nsets, ways)
    };
    let leaf = |level: u32, ctype: CacheType, instr: bool| -> CacheLeaf {
        let (line, sets, ways) = geom(read_ccsidr(level, instr));
        // CLIDR/CCSIDR carry no thread-sharing count; use the level-only
        // arch-info rule (L1 private, L2+ system-wide), matching the arm64
        // `use_arch_info` fallback in cacheinfo.c.
        CacheLeaf {
            level,
            ctype,
            size: Some(line * sets * ways),
            line_size: Some(line),
            sets: Some(sets),
            ways: Some(ways),
            partition: Some(1),
            sharing: SharingScope::Private,
        }
        .with_arch_info_sharing(None)
    };

    let mut leaves = LocalLeaves::new();
    for level in 1u32..=7 {
        let ctype = (clidr >> (3 * (level - 1))) & 0x7;
        let found: &[(CacheType, bool)] = match ctype {
            0 => break, // NoCache: highest level reached.
            1 => &[(CacheType::Instruction, true)],
            2 => &[(CacheType::Data, false)],
            // Separate I and D leaves at this level (data-side selects instr=false).
            3 => &[(CacheType::Data, false), (CacheType::Instruction, true)],
            4 => &[(CacheType::Unified, false)],
            _ => break,
        };
        for &(kind, instr) in found {
            let _ = leaves.push(leaf(level, kind, instr));
        }
    }
    leaves
}

/// loongarch64: `CPUCFG` leaf 16 is the cache-present bitmap; leaves 17.. hold per-leaf
/// geometry (ways = field+1, sets = 1<<field, line = 1<<field). Mirrors
/// `arch/loongarch/mm/cache.c:cpu_cache_init()`.
#[cfg(target_arch = "loongarch64")]
fn read_cache_leaves_loongarch64() -> LocalLeaves {
    use core::arch::asm;

    let cpucfg = |leaf: u32| -> u32 {
        let val: u32;
        unsafe {
            asm!("cpucfg {}, {}", out(reg) val, in(reg) leaf, options(nomem, nostack, preserves_flags));
        }
        val
    };

    let cfg16 = cpucfg(16);
    let mut leaves = LocalLeaves::new();
    let mut cfg_leaf = 0u32; // index into CPUCFG17.. as leaves are found.
    let push = |leaves: &mut LocalLeaves, level: u32, ctype: CacheType, cfg_leaf: &mut u32| {
        let cfg1 = cpucfg(17 + *cfg_leaf);
        let ways = (cfg1 & 0xffff) + 1;
        let sets = 1u32 << ((cfg1 >> 16) & 0xff);
        let line = 1u32 << ((cfg1 >> 24) & 0x7f);
        // CPUCFG carries no thread-sharing count; use the level-only arch-info
        // rule (L1 private, L2+ system-wide).
        let _ = leaves.push(
            CacheLeaf {
                level,
                ctype,
                size: Some(ways * sets * line),
                line_size: Some(line),
                sets: Some(sets),
                ways: Some(ways),
                partition: Some(1),
                sharing: SharingScope::Private,
            }
            .with_arch_info_sharing(None),
        );
        *cfg_leaf += 1;
    };

    // L1: I/U (bit0), unified flag (bit1); D (bit2).
    if cfg16 & (1 << 0) != 0 {
        let ct = if cfg16 & (1 << 1) != 0 {
            CacheType::Unified
        } else {
            CacheType::Instruction
        };
        push(&mut leaves, 1, ct, &mut cfg_leaf);
    }
    if cfg16 & (1 << 2) != 0 {
        push(&mut leaves, 1, CacheType::Data, &mut cfg_leaf);
    }
    // L2 (>>3) and L3 (>>7): IU-present, IU-unify, ...; D-present.
    let mut config = cfg16 >> 3;
    for level in 2u32..=3 {
        if config == 0 {
            break;
        }
        if config & (1 << 0) != 0 {
            let ct = if config & (1 << 1) != 0 {
                CacheType::Unified
            } else {
                CacheType::Instruction
            };
            push(&mut leaves, level, ct, &mut cfg_leaf);
        }
        if config & (1 << 4) != 0 {
            push(&mut leaves, level, CacheType::Data, &mut cfg_leaf);
        }
        config >>= 7;
    }
    leaves
}

/// `/sys/devices/system/cpu/cpu<N>/cache/` - the real cache leaves enumerated for this CPU.
pub(super) struct CpuCacheDir {
    pub(super) fs: Arc<SimpleFs>,
    pub(super) cpu: usize,
}

impl SimpleDirOps for CpuCacheDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        let n = cpu_cache_leaves(self.cpu).len();
        Box::new((0..n).map(|i| Cow::Owned(format!("index{i}"))))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let index = name
            .strip_prefix("index")
            .and_then(|s| s.parse::<usize>().ok())
            .ok_or(VfsError::NotFound)?;
        // Read *this CPU's* pinned leaves, not a live read of the executing PE.
        let leaves = cpu_cache_leaves(self.cpu);
        let leaf = *leaves.get(index).ok_or(VfsError::NotFound)?;
        Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
            self.fs.clone(),
            Arc::new(CpuCacheIndexDir {
                fs: self.fs.clone(),
                cpu: self.cpu,
                leaf,
            }),
        )))
    }
}

/// `/sys/devices/system/cpu/cpu<N>/cache/index<I>/` - one real cache leaf's attributes.
///
/// Only attributes whose value the hardware actually reported are exposed, matching Linux's
/// `cache_default_attrs_is_visible()`. `shared_cpu_map`/`shared_cpu_list` are rebuilt from the
/// leaf's [`SharingScope`]: aarch64 / loongarch64 follow the level-only `use_arch_info` rule
/// (L1 private, L2+ system-wide), while x86 honours its `num_threads_sharing` count (see
/// [`CpuCacheIndexDir::shared_mask`] and [`SharingScope`]).
struct CpuCacheIndexDir {
    fs: Arc<SimpleFs>,
    cpu: usize,
    leaf: CacheLeaf,
}

impl CpuCacheIndexDir {
    /// The set of CPUs sharing this cache, expanding the per-leaf
    /// [`SharingScope`] fixed at detection time (see that type for the Linux
    /// mapping):
    /// - [`Private`](SharingScope::Private) -> the owning CPU only (L1, or an
    ///   x86 leaf with `num_threads_sharing == 1`).
    /// - [`SystemWide`](SharingScope::SystemWide) -> all online CPUs, the
    ///   package domain: the aarch64 / loongarch64 L2+ rule from
    ///   `cache_leaves_are_shared()`, and equally any x86 cache shared beyond
    ///   its owner under this single-package topology.
    ///
    /// At `-smp 1` both scopes collapse to `cpu` alone (bit 0); the runtime
    /// distinction is only observable once more than one CPU is online.
    fn shared_mask(&self) -> String {
        match self.leaf.sharing {
            SharingScope::Private => cpu_bit_mask(self.cpu),
            SharingScope::SystemWide => cpu_hex_mask(),
            // Omitted from `child_names`/`lookup_child`, so never rendered; the
            // owner-only fallback keeps the mask well-formed if ever reached.
            SharingScope::Unknown => cpu_bit_mask(self.cpu),
        }
    }

    fn shared_list(&self) -> String {
        match self.leaf.sharing {
            SharingScope::Private => format!("{}", self.cpu),
            SharingScope::SystemWide => cpu_range_string(),
            SharingScope::Unknown => format!("{}", self.cpu),
        }
    }
}

impl SimpleDirOps for CpuCacheIndexDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        // level and type are always known; geometry only when the arch reported it. `id` is
        // omitted: like Linux, it is only exposed when firmware supplies a real per-cache
        // identifier (`CACHE_ID`), which no arch facility here provides.
        let mut names: Vec<Cow<'a, str>> =
            alloc::vec![Cow::Borrowed("level"), Cow::Borrowed("type"),];
        // `shared_cpu_map`/`shared_cpu_list` only when the exact sharing set is known - the owner
        // (Private) or a provably package-wide set (SystemWide). An [`Unknown`](SharingScope::Unknown)
        // leaf (an x86 subset we cannot pin to specific CPUs) omits them rather than fabricate.
        if !matches!(self.leaf.sharing, SharingScope::Unknown) {
            names.push(Cow::Borrowed("shared_cpu_map"));
            names.push(Cow::Borrowed("shared_cpu_list"));
        }
        if self.leaf.line_size.is_some() {
            names.push(Cow::Borrowed("coherency_line_size"));
        }
        if self.leaf.ways.is_some() {
            names.push(Cow::Borrowed("ways_of_associativity"));
        }
        if self.leaf.sets.is_some() {
            names.push(Cow::Borrowed("number_of_sets"));
        }
        if self.leaf.size.is_some() {
            names.push(Cow::Borrowed("size"));
        }
        if self.leaf.partition.is_some() {
            names.push(Cow::Borrowed("physical_line_partition"));
        }
        Box::new(names.into_iter())
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let leaf = self.leaf;
        // Resolve to an owned body up front so the read closure captures no borrowed `name`.
        // `size` is emitted as `%uK` (KiB) exactly like Linux's `size_show()`.
        let content = match name {
            "level" => format!("{}\n", leaf.level),
            "type" => format!("{}\n", leaf.ctype.as_str()),
            // Omitted (see `child_names`) when the exact sharing set is un-derivable.
            "shared_cpu_map" if !matches!(leaf.sharing, SharingScope::Unknown) => {
                format!("{}\n", self.shared_mask())
            }
            "shared_cpu_list" if !matches!(leaf.sharing, SharingScope::Unknown) => {
                format!("{}\n", self.shared_list())
            }
            "coherency_line_size" => format!("{}\n", leaf.line_size.ok_or(VfsError::NotFound)?),
            "ways_of_associativity" => format!("{}\n", leaf.ways.ok_or(VfsError::NotFound)?),
            "number_of_sets" => format!("{}\n", leaf.sets.ok_or(VfsError::NotFound)?),
            "size" => format!("{}K\n", leaf.size.ok_or(VfsError::NotFound)? / 1024),
            "physical_line_partition" => format!("{}\n", leaf.partition.ok_or(VfsError::NotFound)?),
            _ => return Err(VfsError::NotFound),
        };
        Ok(SimpleFile::new_regular(self.fs.clone(), move || Ok(content.clone())).into())
    }
}

#[cfg(all(test, not(axtest)))]
mod cache_sharing_tests {
    use super::*;

    // Regression guard for the shared_cpu_map cache-ABI fix (#1573). The pre-fix
    // code returned an owner-only scope for an x86 leaf with
    // `1 < num_threads_sharing < online`, under-reporting a genuinely-shared cache;
    // widening it to every CPU (SystemWide) would instead over-report a partial
    // cache as package-wide. The fix maps each leaf against the online count:
    // private (owner), package-wide (all online, provable), or - for a strict
    // subset whose members need APIC grouping this flat tree lacks - Unknown, so
    // `shared_cpu_map`/`shared_cpu_list` are omitted rather than fabricated. The
    // homogeneous -smp 4 QEMU the system test runs on only ever produces private
    // and all-online leaves, so this unit test is what pins the subset mapping.
    #[test]
    fn arch_info_scope_maps_sharing_against_online_count() {
        // 4 online CPUs is the -smp 4 system-suite package size.
        const ONLINE: u32 = 4;

        // L1, or a single-thread leaf at any level, is private.
        assert!(matches!(
            arch_info_scope(1, Some(1), ONLINE),
            SharingScope::Private
        ));
        assert!(matches!(
            arch_info_scope(1, None, ONLINE),
            SharingScope::Private
        ));
        assert!(matches!(
            arch_info_scope(2, Some(1), ONLINE),
            SharingScope::Private
        ));

        // Shared by every online CPU -> the whole package (provably SystemWide);
        // a count at or beyond the online set stays package-wide.
        assert!(matches!(
            arch_info_scope(3, Some(4), ONLINE),
            SharingScope::SystemWide
        ));
        assert!(matches!(
            arch_info_scope(3, Some(8), ONLINE),
            SharingScope::SystemWide
        ));

        // Shared by a strict subset (1 < n < online) -> Unknown: exact members are
        // un-derivable here, so the attribute is omitted, neither owner-only nor all.
        assert!(matches!(
            arch_info_scope(2, Some(2), ONLINE),
            SharingScope::Unknown
        ));
        assert!(matches!(
            arch_info_scope(2, Some(3), ONLINE),
            SharingScope::Unknown
        ));

        // Register-only path (no count) -> higher levels system-wide, L1 private.
        assert!(matches!(
            arch_info_scope(2, None, ONLINE),
            SharingScope::SystemWide
        ));
        assert!(matches!(
            arch_info_scope(3, None, ONLINE),
            SharingScope::SystemWide
        ));
    }
}
