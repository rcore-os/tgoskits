/*
 * VirtIO MMIO transport implementation (see virtio_mmio.h).
 */

#include "virtio_mmio.h"

#include <string.h>

#include "vt_platform.h"

static inline uint32_t vtm_read32(const virtio_mmio_t *v, uint32_t off)
{
    return *(volatile uint32_t *)(v->base + off);
}

static inline void vtm_write32(virtio_mmio_t *v, uint32_t off, uint32_t val)
{
    *(volatile uint32_t *)(v->base + off) = val;
}

int virtio_mmio_probe(virtio_mmio_t *v, uintptr_t mmio_base)
{
    if (v == NULL || mmio_base == 0) {
        return VIRTIO_ERR_INVALID_ARG;
    }
    v->base = (volatile uint8_t *)mmio_base;

    if (vtm_read32(v, VIRTIO_MMIO_MAGIC_VALUE) != VIRTIO_MMIO_MAGIC) {
        return VIRTIO_ERR_BAD_MAGIC;
    }
    if (vtm_read32(v, VIRTIO_MMIO_VERSION) < VIRTIO_MMIO_VERSION_V2) {
        return VIRTIO_ERR_BAD_VERSION;
    }
    v->device_id = vtm_read32(v, VIRTIO_MMIO_DEVICE_ID);
    v->vendor_id = vtm_read32(v, VIRTIO_MMIO_VENDOR_ID);

    virtio_mmio_reset(v);
    return VIRTIO_OK;
}

void virtio_mmio_reset(virtio_mmio_t *v)
{
    vtm_write32(v, VIRTIO_MMIO_STATUS, 0);
    vtm_barrier();
}

uint64_t virtio_mmio_device_features(virtio_mmio_t *v)
{
    uint64_t lo, hi;

    vtm_write32(v, VIRTIO_MMIO_DEVICE_FEATURES_SEL, 0);
    vtm_barrier();
    lo = vtm_read32(v, VIRTIO_MMIO_DEVICE_FEATURES);
    vtm_write32(v, VIRTIO_MMIO_DEVICE_FEATURES_SEL, 1);
    vtm_barrier();
    hi = vtm_read32(v, VIRTIO_MMIO_DEVICE_FEATURES);
    vtm_barrier();

    return lo | (hi << 32);
}

void virtio_mmio_driver_features(virtio_mmio_t *v, uint64_t features)
{
    vtm_write32(v, VIRTIO_MMIO_DRIVER_FEATURES_SEL, 0);
    vtm_barrier();
    vtm_write32(v, VIRTIO_MMIO_DRIVER_FEATURES, (uint32_t)(features & 0xFFFFFFFFu));
    vtm_write32(v, VIRTIO_MMIO_DRIVER_FEATURES_SEL, 1);
    vtm_barrier();
    vtm_write32(v, VIRTIO_MMIO_DRIVER_FEATURES, (uint32_t)(features >> 32));
    vtm_barrier();
}

uint32_t virtio_mmio_status(virtio_mmio_t *v)
{
    return vtm_read32(v, VIRTIO_MMIO_STATUS);
}

void virtio_mmio_set_status(virtio_mmio_t *v, uint32_t bits)
{
    vtm_write32(v, VIRTIO_MMIO_STATUS, bits);
    vtm_barrier();
}

int virtio_mmio_queue_setup(virtio_mmio_t *v, uint16_t index, uint16_t num,
                            uintptr_t desc_phys, uintptr_t avail_phys,
                            uintptr_t used_phys)
{
    if (num == 0 || desc_phys == 0 || avail_phys == 0 || used_phys == 0) {
        return VIRTIO_ERR_INVALID_ARG;
    }

    vtm_write32(v, VIRTIO_MMIO_QUEUE_SEL, index);
    vtm_barrier();
    if (vtm_read32(v, VIRTIO_MMIO_QUEUE_NUM_MAX) < num) {
        return VIRTIO_ERR_QUEUE;
    }
    vtm_write32(v, VIRTIO_MMIO_QUEUE_NUM, num);
    vtm_write32(v, VIRTIO_MMIO_QUEUE_DESC_LOW, (uint32_t)(desc_phys & 0xFFFFFFFFu));
    vtm_write32(v, VIRTIO_MMIO_QUEUE_DESC_HIGH, (uint32_t)(desc_phys >> 32));
    vtm_write32(v, VIRTIO_MMIO_QUEUE_AVAL_LOW, (uint32_t)(avail_phys & 0xFFFFFFFFu));
    vtm_write32(v, VIRTIO_MMIO_QUEUE_AVAL_HIGH, (uint32_t)(avail_phys >> 32));
    vtm_write32(v, VIRTIO_MMIO_QUEUE_USED_LOW, (uint32_t)(used_phys & 0xFFFFFFFFu));
    vtm_write32(v, VIRTIO_MMIO_QUEUE_USED_HIGH, (uint32_t)(used_phys >> 32));
    vtm_barrier();
    vtm_write32(v, VIRTIO_MMIO_QUEUE_READY, 1);
    vtm_barrier();

    return VIRTIO_OK;
}

void virtio_mmio_queue_notify(virtio_mmio_t *v, uint16_t index)
{
    vtm_write32(v, VIRTIO_MMIO_QUEUE_NOTIFY, index);
}

void virtio_mmio_read_config(virtio_mmio_t *v, uint32_t offset, void *out,
                             size_t len)
{
    uint8_t *dst = (uint8_t *)out;
    size_t i;

    /* The config space is a byte-addressable region of 32-bit words. */
    for (i = 0; i < len; i++) {
        uint32_t word_off = (VIRTIO_MMIO_CONFIG_OFFSET + (offset + i)) & ~3u;
        uint32_t shift = ((offset + i) & 3u) * 8u;
        uint32_t word = vtm_read32(v, word_off);
        dst[i] = (uint8_t)(word >> shift);
    }
    vtm_barrier();
}
