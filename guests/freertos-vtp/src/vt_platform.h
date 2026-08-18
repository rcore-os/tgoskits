/*
 * Platform hooks for the FreeRTOS VTP virtio-net driver.
 *
 * The driver only depends on these primitives so it drops into any FreeRTOS
 * AArch64 project:
 *
 *  - vtm_barrier(): full memory barrier. On AArch64 this must be a DMB-ISH;
 *    the default implementation uses the GCC __sync_synchronize() builtin,
 *    which compiles to DMB ISH when targeting aarch64.
 *  - vtm_dma_addr(addr): translate a CPU virtual address to the guest-physical
 *    address the hypervisor DMA path can access. Default is identity (stage-1
 *    identity-mapped or MMU disabled, as in a minimal QEMU AArch64 port).
 *    Override this when the stage-1 page table is not identity-mapped and the
 *    buffers live in a statically-mapped window.
 *
 * Integrators that need different behaviour define VTP_OVERRIDE_PLATFORM before
 * including this header and provide their own vtm_barrier()/vtm_dma_addr().
 */

#ifndef VT_PLATFORM_H
#define VT_PLATFORM_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#ifndef VTP_OVERRIDE_PLATFORM

static inline void vtm_barrier(void)
{
    __sync_synchronize();
}

static inline uintptr_t vtm_dma_addr(const void *virt)
{
    return (uintptr_t)virt;
}

#endif /* VTP_OVERRIDE_PLATFORM */

#ifdef __cplusplus
}
#endif

#endif /* VT_PLATFORM_H */
