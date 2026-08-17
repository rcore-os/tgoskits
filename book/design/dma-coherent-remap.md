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

arm64 selects `DMA_DIRECT_REMAP` in `arch/arm64/Kconfig`. The direct DMA path in
`kernel/dma/direct.c::dma_direct_alloc` allocates physical pages, calls
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

## Chosen design

One coherent allocation has four distinct values and owners:

- allocator address: the cacheable direct-map address used only to release the
  original pages;
- physical/DMA address: stable for the device lifetime;
- CPU alias: a separate uncached kernel VA and the only address exposed for CPU
  access while the coherent allocation is live;
- layout: the allocation extent shared by all three views.

Allocation performs these transitions:

1. allocate physically contiguous pages and calculate the DMA address;
2. clean and invalidate the cache through the allocator address;
3. reserve a free kernel VA and map the physical pages there as
   `READ | WRITE | UNCACHED`;
4. complete cross-CPU TLB invalidation and platform ordering;
5. zero memory through the uncached alias and publish the handle.

Release performs the reverse ownership transition: stop device access, remove
the alias, complete cross-CPU TLB invalidation, and only then free the original
allocator pages. The address-space lock remains held through shootdown so the
alias VA cannot be reused while another CPU may retain a stale translation.

## Failure states

- Before an alias PTE is installed, failure is `NotStarted`; the pages can be
  returned to the allocator.
- After alias installation begins, a shootdown or ordering failure is
  `StateUncertain`; the alias and pages are quarantined.
- During release, any unmap or shootdown failure quarantines the allocation and
  prevents the original pages from being reused.

This deliberately avoids a fallback to in-place direct-map attribute changes.

## Alternatives rejected

- Splitting a huge direct mapping before changing 4 KiB attributes fixes the
  immediate blast radius but still exposes cacheable/uncached alias ownership
  as a global direct-map mutation rather than the arm64 DMA remap model.
- Disabling huge boot mappings loses the intended boot mapping architecture and
  hides the ownership defect.
- Copying coherent data through bounce buffers changes DMA semantics and does
  not solve descriptor visibility for devices that require shared memory.

## Validation

- deterministic page-table test: protecting one base page inside a huge leaf
  changes only that page and preserves neighbor attributes;
- LoongArch regressions: the page-table allocation window excludes every DMW
  direct-map address, targeted invalidation includes global paired entries, and
  the full NVMe-backed QEMU suite exercises consecutive coherent aliases;
- coherent lifecycle tests: the returned CPU alias is distinct from the stored
  allocator address; release unmaps the alias before freeing original pages;
  failure after mapping begins never frees the pages;
- targeted clippy for `page-table-generic`, `dma-api`, `axklib`, `ax-mm`, and
  `ax-runtime`;
- the exact OrangePi 5 Plus `native-hardware-smoke` board case, followed by the
  full Starry OrangePi board suite and the PR CI matrix.

## Non-goals

This change does not add an IOMMU translation domain, make coherent allocation
valid in hard-IRQ context, or change streaming DMA cache-ownership rules.
