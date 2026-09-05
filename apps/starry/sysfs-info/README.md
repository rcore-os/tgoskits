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

The kernel builds `shared_cpu_map` / `shared_cpu_list` from each leaf's sharing
scope, per arch (`drivers/base/cacheinfo.c` + `arch/x86/kernel/cpu/cacheinfo.c`):

- **aarch64 / loongarch64** have no thread-sharing count, so they follow the
  level-only `use_arch_info` rule of `cache_leaves_are_shared()`: an **L1** leaf
  is **private** to its owning CPU, every **L2+** leaf is **shared by all online
  CPUs**.
- **x86** overrides that with `__cache_cpumap_setup()`, whose domain is the set
  of CPUs sharing the cache by `apicid >> order_base_2(num_threads_sharing)`,
  **not** all online CPUs. StarryOS exposes no per-logical-CPU APIC-ID table to
  sysfs (a `cpuN/cache` read may run on any PE), so an x86 leaf with
  `num_threads_sharing > 1` reports only the **owning CPU** - the always-correct
  subset - rather than fabricating an all-online mask that would be wrong on a
  multi-socket / multi-cluster part. `num_threads_sharing == 1` and every L1
  leaf are owner-only regardless.

With only cpu0 online (`-smp 1`) every scope collapses to the single member
cpu0, so every leaf reads `"1"` / `"0"` and this carpet asserts exactly that.
The runtime difference - an aarch64 L2/L3 listing `"0-3"` while L1 stays `"0"` -
is only observable with more than one online CPU and is covered by the `-smp 4`
kernel system-suite regression
`test-suit/starryos/qemu/system/test-sysfs-cpu-topology`, not by this
single-core app run.

The cache geometry is detected once on the primary CPU and served for every
`cpuN`. On QEMU's architecturally identical vCPUs that is each CPU's true
geometry; distinguishing heterogeneous cores (big.LITTLE) would need a
secondary-CPU bring-up hook, which lives in `ax-runtime` (a crate the kernel
depends on, not the reverse) and is out of scope for this sysfs change. This is
a documented homogeneous-SMP assumption, not a claim of per-core heterogeneity.

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
assertions apply. aarch64 and loongarch64 always have readable geometry registers
(CLIDR/CCSIDR, CPUCFG), so `cache/` must be present there.

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
