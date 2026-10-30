# sysfs-info

Doc-grounded carpet test for the StarryOS `/sys` CPU topology, per-CPU cache
geometry, and per-NUMA-node meminfo emitted by
`kernel/src/pseudofs/sysfs.rs`.

The carpet (`programs/sysfs_carpet.c`, cross-compiled static / non-PIE musl)
opens and reads the real sysfs tree on-target, prints every value it reads, and
asserts each one against the semantics documented in
`Documentation/ABI/testing/sysfs-devices-system-cpu` and
`drivers/base/node.c:node_read_meminfo()`.

## What it asserts

Per-CPU cache (`/sys/devices/system/cpu/cpu0/cache/index*`), on
**x86_64 / aarch64 / loongarch64** where the kernel enumerates real cache leaves
from architecture registers (CPUID leaf 4, CLIDR/CCSIDR, CPUCFG):

- `cache/` and `index0/` present, `index0/level == 1`
- `type` in {`Data`, `Instruction`, `Unified`}
- `size` ends in `K` and is `> 0`, and equals `sets * line * ways * physical_line_partition / 1024`
- `coherency_line_size`, `number_of_sets`, `ways_of_associativity` all `> 0`
- every leaf's `shared_cpu_map == "1"` (hex bit 0) and `shared_cpu_list == "0"`
  under single core, and at least one L1 leaf is present

### shared_cpu_map: what -smp 1 can and cannot show

Every leaf carries `shared_cpu_map` and `shared_cpu_list`, built the way Linux
builds them (`drivers/base/cacheinfo.c` + `arch/x86/kernel/cpu/cacheinfo.c`):

- **aarch64 / loongarch64** have no thread-sharing count, so they follow the
  `use_arch_info` rule of `cache_leaves_are_shared()`: an **L1** leaf is
  **private** to its owning CPU, every **L2+** leaf is **shared by every CPU
  with cache info**. The x86 leaf 0x2 fallback carries no count either and
  follows the same rule.
- **x86** follows `__cache_cpumap_setup()`. A leaf whose CPUID leaf 4
  `num_threads_sharing` is 1 is private. Any other leaf, L1 included, is shared
  by the CPUs whose APIC id equals the owner's once the low
  `get_count_order(num_threads_sharing)` bits are dropped. Each CPU records its
  APIC id while it samples its own cache registers, so SMT siblings share their
  core's L1/L2 and each LLC lists only the CPUs of its own domain.

With only cpu0 online (`-smp 1`) every set is cpu0 alone, so every leaf reads
`"1"` / `"0"` and this carpet asserts exactly that. Multi-CPU sets are covered by
the `-smp 4` kernel system-suite regression
`test-suit/starryos/qemu/system/test-sysfs-cpu-topology` (L2+ on aarch64 /
loongarch64 and the x86 L3 list `"0-3"`, every L1 stays private) and by the
`cache_sharing_tests` host tests in `cache.rs`, which pin SMT pairs, LLC domains
with non-contiguous APIC ids, and a CPU without cache info.

`topology/` still models a single package without SMT, so on SMT hardware
`thread_siblings` lists only the CPU itself while its L1 `shared_cpu_list` names
both threads.

The cache geometry is sampled on each CPU itself: the primary reads its own
registers and every other CPU is read through `run_on_cpu_sync`, so heterogeneous
cores report their own geometry. A CPU whose sampling call does not complete
keeps an empty slot and its `cpuN/cache` is absent; it is never filled with
another CPU's data.

On **riscv64** there is no cache-geometry register source (Linux uses the device
tree only, and StarryOS carries no DT cacheinfo parser), so the kernel omits
`cache/` rather than fabricate values. The carpet asserts `cache/` is **absent**
on riscv64 and does not fail for the missing directory.

**x86_64 caveat:** the kernel reads CPUID leaf 4 (deterministic cache
parameters). Under QEMU/TCG the guest CPU model may leave leaf 4 unpopulated
(cache_type=0 at subleaf 0), in which case the kernel omits `cache/` - the same
"unavailable => absent" outcome as riscv64, and correct (no fabrication). The
carpet therefore treats an absent `cache/` on x86 as an informational caveat, not
a failure; on real x86 hardware (or a QEMU model that populates leaf 4) the cache
assertions apply. AMD and Hygon processors report their caches through leaf
0x8000001D instead (`amd_fill_cpuid4_info()` in Linux); that leaf is not read
yet, so `cache/` is absent on them as well. aarch64 and loongarch64 always have
readable geometry registers (CLIDR/CCSIDR, CPUCFG), so `cache/` must be present
there.

Per-node meminfo (`/sys/devices/system/node/node0/meminfo`), all arches:

- `MemTotal > 0`, `MemFree > 0`, `MemFree < MemTotal`, `MemUsed == MemTotal - MemFree`

The `MemFree < MemTotal` and `MemUsed > 0` checks specifically prove the value is
the live allocator gauge, not the earlier placeholder that reported
`MemFree == MemTotal` / `MemUsed == 0`.

Per-CPU topology (`/sys/devices/system/cpu/cpu0/topology/*`), all arches:

- `core_id`, `physical_package_id`, `core_cpus`, `core_cpus_list`,
  `package_cpus`, `thread_siblings`, `thread_siblings_list` present + parseable
- single-core: `core_cpus_list == "0"`, `thread_siblings_list == "0"`

The final line is `SYSFS_CARPET OK=<n>/<n>` followed by
`SYSFS_CARPET TEST PASSED` (the qemu `success_regex`) or
`SYSFS_CARPET TEST FAILED`.

## Run

```
cargo xtask starry app qemu -t sysfs-info --arch x86_64
cargo xtask starry app qemu -t sysfs-info --arch aarch64
cargo xtask starry app qemu -t sysfs-info --arch riscv64
cargo xtask starry app qemu -t sysfs-info --arch loongarch64
```

All configs run single core (`-smp 1`). `prebuild.sh` cross-compiles the carpet
with the per-arch musl toolchain (`-static -no-pie`; `-no-pie` is required so
riscv64 musl does not emit a static-PIE binary the loader rejects) and stages it
at `/usr/bin/sysfs-carpet`, launched by `/usr/bin/sysfs-info.sh`.
