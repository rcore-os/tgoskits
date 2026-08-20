/*
 * VirtIO-net device driver for the FreeRTOS VTP guest.
 *
 * A minimal split-ring virtio-net driver matching the Axvisor device
 * (virtualization/axvirtio-net + axvirtio-common). Negotiated features are
 * VIRTIO_F_VERSION_1 | VIRTIO_NET_F_MAC | VIRTIO_NET_F_STATUS. One queue pair
 * (RX=0, TX=1), 64 descriptors, split rings, no indirect descriptors, no
 * event index. Because VIRTIO_F_VERSION_1 is negotiated, both the TX header
 * the driver prepends and the RX header the device writes are 12 bytes
 * (10-byte virtio_net_hdr + num_buffers), matching the workspace's modern
 * virtio-drivers guest ABI (see axvirtio-net/src/constants.rs).
 *
 * All ring memory and buffers are statically allocated inside the device
 * struct, so no allocator is required. Buffers must be DMA-visible at their
 * guest-physical address; vt_platform.h controls the virt→phys translation.
 */

#ifndef VIRTIO_NET_H
#define VIRTIO_NET_H

#include <stddef.h>
#include <stdint.h>

#include "virtio_mmio.h"

#ifdef __cplusplus
extern "C" {
#endif

#define VIRTIO_NET_F_MAC    (1ULL << 5)
#define VIRTIO_NET_F_STATUS (1ULL << 16)

#define VIRTIO_NET_HDR_SIZE 12u /* 10-byte base + num_buffers, modern ABI */

#define VIRTIO_NET_CFG_MAC   0x00u /* 6 bytes at config offset 0 */
#define VIRTIO_NET_CFG_STATUS 0x06u /* u16 link status */

#define VIRTIO_NET_QUEUE_RX 0u
#define VIRTIO_NET_QUEUE_TX 1u

#define VIRTIO_NET_QUEUE_SIZE   64u
#define VIRTIO_NET_RX_BUFFER_SZ 2048u
#define VIRTIO_NET_TX_BUFFER_SZ 2048u

#define VIRTIO_NET_MTU 1500u

#define VIRTIO_NET_OK 0
#define VIRTIO_NET_ERR_QUEUE_FULL (-1)
#define VIRTIO_NET_ERR_NONE       (-2)

typedef struct __attribute__((packed)) {
    uint64_t addr;
    uint32_t len;
    uint16_t flags;
    uint16_t next;
} vt_virtq_desc_t;

typedef struct __attribute__((packed)) {
    uint16_t flags;
    uint16_t idx;
    uint16_t ring[VIRTIO_NET_QUEUE_SIZE];
    uint16_t used_event;
} vt_virtq_avail_t;

typedef struct __attribute__((packed)) {
    uint32_t id;
    uint32_t len;
} vt_virtq_used_elem_t;

typedef struct __attribute__((packed)) {
    uint16_t flags;
    uint16_t idx;
    vt_virtq_used_elem_t ring[VIRTIO_NET_QUEUE_SIZE];
    uint16_t avail_event;
} vt_virtq_used_t;

/* Descriptor flags. */
#define VIRTQ_DESC_F_NEXT    1u
#define VIRTQ_DESC_F_WRITE   2u
#define VIRTQ_DESC_F_INDIRECT 4u

typedef struct {
    virtio_mmio_t transport;
    vt_virtq_desc_t rx_desc[VIRTIO_NET_QUEUE_SIZE] __attribute__((aligned(16)));
    vt_virtq_avail_t rx_avail __attribute__((aligned(2)));
    vt_virtq_used_t rx_used __attribute__((aligned(4)));
    uint8_t rx_buf[VIRTIO_NET_QUEUE_SIZE][VIRTIO_NET_RX_BUFFER_SZ]
        __attribute__((aligned(16)));
    uint16_t rx_avail_idx;
    uint16_t rx_last_used;

    vt_virtq_desc_t tx_desc[VIRTIO_NET_QUEUE_SIZE] __attribute__((aligned(16)));
    vt_virtq_avail_t tx_avail __attribute__((aligned(2)));
    vt_virtq_used_t tx_used __attribute__((aligned(4)));
    uint8_t tx_buf[VIRTIO_NET_QUEUE_SIZE][VIRTIO_NET_TX_BUFFER_SZ]
        __attribute__((aligned(16)));
    uint16_t tx_avail_idx;
    uint16_t tx_last_used;
    uint16_t tx_free_head;
    uint16_t tx_free_count;

    uint8_t mac[6];
    uint64_t features;
    int ready;
} virtio_net_t;

/*
 * Probe + initialize the virtio-net device at `mmio_base`.
 * Returns VIRTIO_NET_OK or a negative error. On success the RX queue is
 * pre-posted and the device is DRIVER_OK.
 */
int virtio_net_init(virtio_net_t *dev, uintptr_t mmio_base);

/* Link status from config space (VIRTIO_NET_S_LINK_UP). */
uint16_t virtio_net_link_status(const virtio_net_t *dev);

const uint8_t *virtio_net_mac(const virtio_net_t *dev);

/*
 * Transmit one Ethernet frame (no virtio header). Copies the frame into a TX
 * slot, prepends the 12-byte header, and notifies the device.
 * Returns VIRTIO_NET_OK or VIRTIO_NET_ERR_QUEUE_FULL.
 */
int virtio_net_send(virtio_net_t *dev, const uint8_t *frame, uint16_t len);

/*
 * Poll for one received Ethernet frame. On success copies the frame (12-byte
 * header stripped) into `out`/`cap` and returns the frame length; returns
 * VIRTIO_NET_ERR_NONE when no frame is available.
 */
int virtio_net_recv(virtio_net_t *dev, uint8_t *out, uint16_t cap);

#ifdef __cplusplus
}
#endif

#endif /* VIRTIO_NET_H */
