/*
 * VirtIO-net device implementation (see virtio_net.h).
 */

#include "virtio_net.h"

#include <string.h>

#include "vt_platform.h"

static uint16_t le16(const uint8_t *p)
{
    return (uint16_t)((uint16_t)p[0] | ((uint16_t)p[1] << 8));
}

/* Reclaim TX descriptors returned by the device in the used ring. */
static void virtio_net_reclaim_tx(virtio_net_t *dev)
{
    /* Acquire: the device updated the used index before the ring elements. */
    vtm_barrier();
    while (dev->tx_used.idx != dev->tx_last_used) {
        vt_virtq_used_elem_t *elem =
            &dev->tx_used.ring[dev->tx_last_used % VIRTIO_NET_QUEUE_SIZE];
        uint16_t id = (uint16_t)elem->id;

        /* Push id back onto the free list (desc.next is unused while free). */
        if (id < VIRTIO_NET_QUEUE_SIZE) {
            dev->tx_desc[id].next = dev->tx_free_head;
            dev->tx_free_head = id;
            dev->tx_free_count++;
        }
        dev->tx_last_used++;
    }
}

int virtio_net_init(virtio_net_t *dev, uintptr_t mmio_base)
{
    uint64_t device_features;
    uint8_t mac[6];
    unsigned i;
    int rc;

    if (dev == NULL) {
        return VIRTIO_NET_ERR_QUEUE_FULL;
    }
    memset(dev, 0, sizeof(*dev));

    rc = virtio_mmio_probe(&dev->transport, mmio_base);
    if (rc < 0) {
        return rc;
    }
    if (dev->transport.device_id != 1 /* VIRTIO_ID_NET */) {
        return VIRTIO_ERR_BAD_DEVICE;
    }

    virtio_mmio_set_status(&dev->transport, VIRTIO_STATUS_ACKNOWLEDGE);
    virtio_mmio_set_status(&dev->transport, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER);

    /* Negotiate only features the driver implements. */
    device_features = virtio_mmio_device_features(&dev->transport);
    dev->features = device_features &
                    (VIRTIO_F_VERSION_1 | VIRTIO_NET_F_MAC | VIRTIO_NET_F_STATUS);
    if ((dev->features & VIRTIO_F_VERSION_1) == 0) {
        virtio_mmio_set_status(&dev->transport, VIRTIO_STATUS_FAILED);
        return VIRTIO_ERR_FEATURES;
    }
    virtio_mmio_driver_features(&dev->transport, dev->features);
    virtio_mmio_set_status(&dev->transport, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER |
                                                VIRTIO_STATUS_FEATURES_OK);
    if ((virtio_mmio_status(&dev->transport) & VIRTIO_STATUS_FEATURES_OK) == 0) {
        virtio_mmio_set_status(&dev->transport, VIRTIO_STATUS_FAILED);
        return VIRTIO_ERR_FEATURES;
    }

    /* Split-ring queue setup for RX (0) and TX (1). */
    rc = virtio_mmio_queue_setup(&dev->transport, VIRTIO_NET_QUEUE_RX,
                                 VIRTIO_NET_QUEUE_SIZE,
                                 vtm_dma_addr(dev->rx_desc),
                                 vtm_dma_addr(&dev->rx_avail),
                                 vtm_dma_addr(&dev->rx_used));
    if (rc < 0) {
        virtio_mmio_set_status(&dev->transport, VIRTIO_STATUS_FAILED);
        return rc;
    }
    rc = virtio_mmio_queue_setup(&dev->transport, VIRTIO_NET_QUEUE_TX,
                                 VIRTIO_NET_QUEUE_SIZE,
                                 vtm_dma_addr(dev->tx_desc),
                                 vtm_dma_addr(&dev->tx_avail),
                                 vtm_dma_addr(&dev->tx_used));
    if (rc < 0) {
        virtio_mmio_set_status(&dev->transport, VIRTIO_STATUS_FAILED);
        return rc;
    }

    /* Pre-post all RX buffers. */
    for (i = 0; i < VIRTIO_NET_QUEUE_SIZE; i++) {
        dev->rx_desc[i].addr = vtm_dma_addr(dev->rx_buf[i]);
        dev->rx_desc[i].len = VIRTIO_NET_RX_BUFFER_SZ;
        dev->rx_desc[i].flags = VIRTQ_DESC_F_WRITE;
        dev->rx_desc[i].next = 0;
        dev->rx_avail.ring[i] = (uint16_t)i;
    }
    vtm_barrier();
    dev->rx_avail.idx = VIRTIO_NET_QUEUE_SIZE;
    dev->rx_avail_idx = VIRTIO_NET_QUEUE_SIZE;
    dev->rx_last_used = 0;
    vtm_barrier();
    virtio_mmio_queue_notify(&dev->transport, VIRTIO_NET_QUEUE_RX);

    /* TX free descriptor list: descriptors 0..N-1 chained via desc.next. */
    for (i = 0; i < VIRTIO_NET_QUEUE_SIZE - 1; i++) {
        dev->tx_desc[i].next = (uint16_t)(i + 1);
    }
    dev->tx_desc[VIRTIO_NET_QUEUE_SIZE - 1].next = 0;
    dev->tx_free_head = 0;
    dev->tx_free_count = VIRTIO_NET_QUEUE_SIZE;
    dev->tx_avail_idx = 0;
    dev->tx_last_used = 0;

    /* Driver is now ready. */
    virtio_mmio_set_status(&dev->transport, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER |
                                                VIRTIO_STATUS_FEATURES_OK |
                                                VIRTIO_STATUS_DRIVER_OK);

    /* MAC from config space (only valid when VIRTIO_NET_F_MAC negotiated). */
    if (dev->features & VIRTIO_NET_F_MAC) {
        virtio_mmio_read_config(&dev->transport, VIRTIO_NET_CFG_MAC, mac, 6);
        memcpy(dev->mac, mac, 6);
    }

    dev->ready = 1;
    return VIRTIO_NET_OK;
}

uint16_t virtio_net_link_status(const virtio_net_t *dev)
{
    uint8_t raw[2];

    if ((dev->features & VIRTIO_NET_F_STATUS) == 0) {
        return 1; /* status feature not negotiated → assume link up */
    }
    virtio_mmio_read_config((virtio_mmio_t *)&dev->transport,
                            VIRTIO_NET_CFG_STATUS, raw, 2);
    return le16(raw);
}

const uint8_t *virtio_net_mac(const virtio_net_t *dev)
{
    return dev->mac;
}

int virtio_net_send(virtio_net_t *dev, const uint8_t *frame, uint16_t len)
{
    uint16_t id;

    if (dev == NULL || frame == NULL || len == 0 ||
        len + VIRTIO_NET_HDR_SIZE > VIRTIO_NET_TX_BUFFER_SZ) {
        return VIRTIO_NET_ERR_QUEUE_FULL;
    }

    virtio_net_reclaim_tx(dev);
    if (dev->tx_free_count == 0) {
        return VIRTIO_NET_ERR_QUEUE_FULL;
    }

    id = dev->tx_free_head;
    dev->tx_free_head = dev->tx_desc[id].next;
    dev->tx_free_count--;

    /* 12-byte zero virtio_net_hdr (no offloads) + Ethernet frame. */
    memset(dev->tx_buf[id], 0, VIRTIO_NET_HDR_SIZE);
    memcpy(dev->tx_buf[id] + VIRTIO_NET_HDR_SIZE, frame, len);

    dev->tx_desc[id].addr = vtm_dma_addr(dev->tx_buf[id]);
    dev->tx_desc[id].len = VIRTIO_NET_HDR_SIZE + (uint32_t)len;
    dev->tx_desc[id].flags = 0; /* device-readable */
    dev->tx_desc[id].next = 0;
    vtm_barrier();

    dev->tx_avail.ring[dev->tx_avail_idx % VIRTIO_NET_QUEUE_SIZE] = id;
    vtm_barrier();
    dev->tx_avail.idx++;
    dev->tx_avail_idx++;
    vtm_barrier();

    virtio_mmio_queue_notify(&dev->transport, VIRTIO_NET_QUEUE_TX);
    return VIRTIO_NET_OK;
}

int virtio_net_recv(virtio_net_t *dev, uint8_t *out, uint16_t cap)
{
    vt_virtq_used_elem_t *elem;
    uint16_t id;
    uint32_t written;
    uint32_t frame_len;

    if (dev == NULL || out == NULL || cap == 0) {
        return VIRTIO_NET_ERR_NONE;
    }

    vtm_barrier();
    if (dev->rx_used.idx == dev->rx_last_used) {
        return VIRTIO_NET_ERR_NONE;
    }

    elem = &dev->rx_used.ring[dev->rx_last_used % VIRTIO_NET_QUEUE_SIZE];
    id = (uint16_t)elem->id;
    written = elem->len;

    /* Re-post the buffer immediately so the device can reuse it. */
    dev->rx_avail.ring[dev->rx_avail_idx % VIRTIO_NET_QUEUE_SIZE] = id;
    vtm_barrier();
    dev->rx_avail.idx++;
    dev->rx_avail_idx++;
    vtm_barrier();
    virtio_mmio_queue_notify(&dev->transport, VIRTIO_NET_QUEUE_RX);
    dev->rx_last_used++;

    if (written < VIRTIO_NET_HDR_SIZE) {
        return VIRTIO_NET_ERR_NONE; /* corrupt short frame: drop */
    }
    frame_len = written - VIRTIO_NET_HDR_SIZE;
    if (frame_len > cap) {
        return VIRTIO_NET_ERR_NONE; /* caller buffer too small: drop */
    }
    memcpy(out, dev->rx_buf[id] + VIRTIO_NET_HDR_SIZE, frame_len);
    return (int)frame_len;
}
