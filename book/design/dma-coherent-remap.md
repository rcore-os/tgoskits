# DMA-coherent remap ownership

## Problem

ArceOS previously implemented `alloc_coherent` by changing the cache attributes
of the allocator's kernel direct mapping in place. Boot direct mappings may now
use 2 MiB or 1 GiB block entries. A 4 KiB DMA allocation inside such a block can
therefore change the attributes of unrelated pages, while later allocations can
change the same block back. This permits cached and uncached users to disagree
about one physical range and caused the Rockchip SDHCI EXT_CSD ADMA request to
fail after huge boot mappings were enabled.

The users are DMA drivers that receive CPU and device addresses from
`dma-api`, especially descriptor rings allocated through `CoherentArray`.

## Linux v7.1 reference model

Linux first establishes the device property with `dev_is_dma_coherent`. OF
walks the DMA-parent chain for `dma-coherent` or `dma-noncoherent`. Each step
first follows the `interconnect-names = "dma-mem"` entry, whose specifier width
comes from the provider's `#interconnect-cells`; an absent or malformed entry
falls back to the ordinary DT parent. The final fallback is the architecture's
`dma_default_coherent`. ACPI propagates the first `_CCA` declared by an ACPI
device ancestor and ignores descendant overrides. Supported architectures
other than arm64 default a fully missing `_CCA` chain to coherent, while arm64
requires a firmware value.

`kernel/dma/direct.c::dma_direct_alloc` branches on that property. A coherent
device normally receives the allocator's existing `page_address` direct
mapping. A non-coherent device uses an architecture allocator, a global
coherent pool, a direct mapping whose attributes can safely be changed, or a
separate uncached remap. Allocation and release select the same branch.

arm64 selects `DMA_DIRECT_REMAP` in `arch/arm64/Kconfig`. For its non-coherent
remap branch, the direct DMA path allocates physical pages, calls
`arch_dma_prep_coherent`, and creates a separate coherent CPU mapping with
`dma_common_contiguous_remap`. The DMA handle remains the physical address of
the original pages. `dma_direct_free` removes a vmalloc alias before freeing the
pages identified by the DMA address.

`kernel/dma/remap.c` explicitly describes this remap as a sleeping-context
operation and pairs `vmap` with `vunmap`. PREEMPT_RT does not replace this
ownership model with direct-map attribute changes.

Linux arm64 also calls `split_kernel_leaf_mapping` before partial kernel
page-attribute updates (`arch/arm64/mm/pageattr.c`). Thus the generic page-table
contract must split block mappings crossed by protection boundaries even though
coherent DMA no longer depends on direct-map protection.

On LoongArch, Linux derives `vm_map_base` from the raw CPUCFG VABITS sign-bit
index and keeps `vmalloc` in XKVRANGE, separate from the DMW direct map that
bypasses page-table walking. LoongArch TLB entries cover even/odd page pairs;
Linux aligns kernel range invalidations to the pair and uses
`INVTLB_ADDR_GTRUE_OR_ASID`, which includes global kernel translations. The
runtime alias path must preserve both rules or a valid PTE can remain hidden by
a stale paired TLB entry.

Linux SWIOTLB also separates bounce-buffer copying from cache maintenance.
Bounce synchronization copies for coherent and non-coherent devices, while
architecture cache synchronization runs only for non-coherent devices.

## Chosen design

`DeviceDma` owns an explicit `DmaCoherency` supplied by the firmware or bus
probe. Every FDT DMA consumer uses the same DMA-parent resolver; PCI hosts also
propagate that result to every enumerated endpoint. ACPI devices and PCI hosts
use inherited `_CCA`, including the Linux architecture default when `_CCA` is
optional. Driver cores and the rd-net queue layer receive this device-scoped
capability rather than constructing a new global DMA capability and losing the
firmware or bus property. RDIF block `QueueLimits` carries both domain identity
and coherency into the ax-fs request allocator; rd-net retains the same pair for
its packet pools. Per-queue masks narrow `DmaConstraints` without changing
coherency or DMA-domain identity, and request validation rejects a prepared
buffer from a different domain or coherency contract.

For a coherent device, `alloc_coherent` uses the allocator's contiguous direct
mapping. Its CPU address and allocator address are identical, explicit cache
ownership transfers are no-ops, and release returns the same mapping to the
contiguous allocator.

For a non-coherent device using the remap backend, one allocation has four
distinct values and owners:

- allocator address: the cacheable direct-map address used only to release the
  original pages;
- physical/DMA address: stable for the device lifetime;
- CPU alias: a separate uncached kernel VA and the only address exposed for CPU
  access while the coherent allocation is live;
- layout: the allocation extent shared by all three views.

Non-coherent alias allocation performs these transitions:

1. allocate physically contiguous pages and calculate the DMA address;
2. clean and invalidate the cache through the allocator address;
3. reserve a free kernel VA and map the physical pages there as
   `READ | WRITE | UNCACHED`;
4. complete cross-CPU TLB invalidation and platform ordering;
5. zero memory through the uncached alias and publish the handle.

Alias release performs the reverse ownership transition: stop device access,
remove the alias, complete cross-CPU TLB invalidation, and only then free the
original allocator pages. The address-space lock remains held through shootdown
so the alias VA cannot be reused while another CPU may retain a stale
translation.

Streaming bounce synchronization keeps two independent decisions: bounce
copies occur for both coherent and non-coherent devices, while cache maintenance
occurs only for non-coherent devices.

## Failure states

- Before a non-coherent alias PTE is installed, failure is `NotStarted`; the
  pages can be returned to the allocator.
- After alias installation begins, a shootdown or ordering failure is
  `StateUncertain`; the alias and pages are quarantined.
- During alias release, any unmap or shootdown failure quarantines the
  allocation and prevents the original pages from being reused.

This deliberately avoids an error-driven fallback to a direct mapping or an
in-place direct-map attribute change. The direct-mapping branch is selected
only from an explicit coherent-device property.

## Alternatives rejected

- Splitting a huge direct mapping before changing 4 KiB attributes fixes the
  immediate blast radius but still exposes cacheable/uncached alias ownership
  as a global direct-map mutation rather than the arm64 DMA remap model.
- Disabling huge boot mappings loses the intended boot mapping architecture and
  hides the ownership defect.
- Copying coherent data through bounce buffers changes DMA semantics and does
  not solve descriptor visibility for devices that require shared memory.
- Treating every device as non-coherent forces coherent PCI devices through the
  remap allocator and can fail even when physical memory is available.
- Treating every ACPI PCI host as coherent ignores `_CCA` on arm64 and other
  firmware-described non-coherent systems.

## Validation

- deterministic page-table test: protecting one base page inside a huge leaf
  changes only that page and preserves neighbor attributes;
- LoongArch regressions: the page-table allocation window excludes every DMW
  direct-map address, targeted invalidation includes global paired entries, and
  the full NVMe-backed QEMU suite exercises consecutive non-coherent aliases;
- coherent lifecycle tests: a non-coherent allocation retains distinct CPU
  alias and allocator addresses; release unmaps the alias before freeing the
  original pages; failure after mapping begins never frees the pages;
- coherent-device regression: the RISC-V PCI host's `dma-coherent` property
  selects the direct allocator mapping, so the exact 256 MiB NVMe-backed
  ArceOS QEMU suite no longer fails with alias-VA `NoMemory`;
- streaming regression: coherent bounce buffers still copy in both directions
  but perform zero cache-maintenance operations;
- firmware propagation regressions: variable-width `dma-mem` interconnect
  entries resolve the intended provider, malformed entries select the ordinary
  parent path, ancestor `_CCA` overrides a conflicting child, and a wholly
  missing ACPI chain remains unspecified for the architecture consumer;
- network regression: queue-specific DMA masks preserve the NIC's coherency
  property rather than rebuilding a non-coherent global device;
- block regression: controller queue limits preserve the device coherency into
  the filesystem request allocator, and mismatched prepared buffers are
  rejected before hardware ownership begins;
- targeted clippy for `page-table-generic`, `dma-api`, `axklib`, `ax-mm`,
  `ax-runtime`, `rdrive`, `rdif-pcie`, and the affected PCI drivers;
- the exact OrangePi 5 Plus `native-hardware-smoke` board case, followed by the
  full Starry OrangePi board suite and the PR CI matrix.

## Non-goals

This change does not add an IOMMU translation domain, make coherent allocation
valid in hard-IRQ context, or replace the streaming API's explicit
`prepare_for_device` / `complete_for_cpu` ownership contract with Linux's
implicit map/unmap synchronization contract.
