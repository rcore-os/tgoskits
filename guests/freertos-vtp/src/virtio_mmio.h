/*
 * VirtIO MMIO transport for the FreeRTOS VTP guest.
 *
 * Implements the VirtIO 1.1 MMIO transport registers (see
 * virtualization/axvirtio-common/src/constants/mmio.rs for the reference
 * offsets, which match the virtio spec virtio-mmio section). This layer knows
 * nothing about virtio-net: it handles device reset, feature negotiation,
 * status bits, queue setup and notification.
 *
 * Guest physical address requirement: every buffer exposed to the device
 * (descriptor table, available/used rings, net buffers) must be physically
 * contiguous and at a guest-physical address the hypervisor's DMA grant can
 * reach. With an identity-mapped stage-1 this is the buffer's virtual address.
 */

#ifndef VIRTIO_MMIO_H
#define VIRTIO_MMIO_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* --- MMIO register offsets (virtio 1.1 spec / axvirtio-common) ---------- */
#define VIRTIO_MMIO_MAGIC_VALUE          0x000u
#define VIRTIO_MMIO_VERSION              0x004u
#define VIRTIO_MMIO_DEVICE_ID            0x008u
#define VIRTIO_MMIO_VENDOR_ID            0x00cu
#define VIRTIO_MMIO_DEVICE_FEATURES      0x010u
#define VIRTIO_MMIO_DEVICE_FEATURES_SEL  0x014u
#define VIRTIO_MMIO_DRIVER_FEATURES      0x020u
#define VIRTIO_MMIO_DRIVER_FEATURES_SEL  0x024u
#define VIRTIO_MMIO_QUEUE_SEL            0x030u
#define VIRTIO_MMIO_QUEUE_NUM_MAX        0x034u
#define VIRTIO_MMIO_QUEUE_NUM            0x038u
#define VIRTIO_MMIO_QUEUE_READY          0x044u
#define VIRTIO_MMIO_QUEUE_NOTIFY         0x050u
#define VIRTIO_MMIO_INTERRUPT_STATUS     0x060u
#define VIRTIO_MMIO_INTERRUPT_ACK        0x064u
#define VIRTIO_MMIO_STATUS               0x070u
#define VIRTIO_MMIO_QUEUE_DESC_LOW       0x080u
#define VIRTIO_MMIO_QUEUE_DESC_HIGH      0x084u
#define VIRTIO_MMIO_QUEUE_AVAL_LOW       0x090u
#define VIRTIO_MMIO_QUEUE_AVAL_HIGH      0x094u
#define VIRTIO_MMIO_QUEUE_USED_LOW       0x0a0u
#define VIRTIO_MMIO_QUEUE_USED_HIGH      0x0a4u
#define VIRTIO_MMIO_CONFIG_GENERATION    0x0fcu
#define VIRTIO_MMIO_CONFIG_OFFSET        0x100u

/* --- Magic / version ------------------------------------------------------ */
#define VIRTIO_MMIO_MAGIC      0x74726976u /* "virt" LE */
#define VIRTIO_MMIO_VERSION_V2 2u
#define VIRTIO_VENDOR_ID       0x1AF4u

/* --- Device status bits --------------------------------------------------- */
#define VIRTIO_STATUS_ACKNOWLEDGE   0x01u
#define VIRTIO_STATUS_DRIVER        0x02u
#define VIRTIO_STATUS_DRIVER_OK     0x04u
#define VIRTIO_STATUS_FEATURES_OK   0x08u
#define VIRTIO_STATUS_NEEDS_RESET   0x40u
#define VIRTIO_STATUS_FAILED        0x80u

/* --- Generic feature bits ------------------------------------------------- */
#define VIRTIO_F_INDIRECT_DESC      (1ULL << 28)
#define VIRTIO_F_VERSION_1          (1ULL << 32)
#define VIRTIO_F_RING_RESET         (1ULL << 40)

/* --- Return codes ---------------------------------------------------------- */
#define VIRTIO_OK 0
#define VIRTIO_ERR_BAD_MAGIC   (-1)
#define VIRTIO_ERR_BAD_VERSION (-2)
#define VIRTIO_ERR_BAD_DEVICE  (-3)
#define VIRTIO_ERR_FEATURES    (-4)
#define VIRTIO_ERR_QUEUE       (-5)
#define VIRTIO_ERR_INVALID_ARG (-6)

/* Opaque MMIO transport state. */
typedef struct {
    volatile uint8_t *base;
    uint32_t device_id;
    uint32_t vendor_id;
} virtio_mmio_t;

/*
 * Probe + reset a VirtIO MMIO device at `mmio_base`.
 * Verifies magic/version, captures device/vendor id, and resets the device.
 * Returns VIRTIO_OK or a negative error.
 */
int virtio_mmio_probe(virtio_mmio_t *v, uintptr_t mmio_base);

/* Reset the device (writes STATUS = 0). */
void virtio_mmio_reset(virtio_mmio_t *v);

/* Full 64-bit device feature bitmap. */
uint64_t virtio_mmio_device_features(virtio_mmio_t *v);

/* Write the driver-selected 64-bit feature bitmap. */
void virtio_mmio_driver_features(virtio_mmio_t *v, uint64_t features);

uint32_t virtio_mmio_status(virtio_mmio_t *v);
void virtio_mmio_set_status(virtio_mmio_t *v, uint32_t bits);

/*
 * Set up split virtqueue `index` with `num` descriptors whose descriptor
 * table, available ring and used ring live at the given guest-physical
 * addresses. Returns VIRTIO_OK or a negative error.
 */
int virtio_mmio_queue_setup(virtio_mmio_t *v, uint16_t index, uint16_t num,
                            uintptr_t desc_phys, uintptr_t avail_phys,
                            uintptr_t used_phys);

/* Notify the device that queue `index` has new available entries. */
void virtio_mmio_queue_notify(virtio_mmio_t *v, uint16_t index);

/* Read `len` bytes from the device config space (little-endian words). */
void virtio_mmio_read_config(virtio_mmio_t *v, uint32_t offset, void *out,
                             size_t len);

#ifdef __cplusplus
}
#endif

#endif /* VIRTIO_MMIO_H */
