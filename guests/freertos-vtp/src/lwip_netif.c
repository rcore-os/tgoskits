/*
 * lwIP netif glue for the FreeRTOS VTP guest.
 *
 * Bridges the virtio-net driver (guests/freertos-vtp/src/virtio_net.c) into
 * lwIP: a dedicated RX task calls vt_netif_poll() to drain frames from the
 * virtio RX queue into ethernet_input(), and linkoutput sends frames through
 * virtio_net_send(). The netif owns a fixed IPv4 address (10.0.2.16) matching
 * the StarryOS peer (10.0.2.15) on the Axvisor L2 switch.
 *
 * Requires lwIP with LWIP_ARP=1, LWIP_ETHERNET=1, LWIP_IPV4=1.
 */

#include <string.h>

#include "vt_platform.h"
#include "virtio_net.h"

#include "lwip/opt.h"
#include "lwip/def.h"
#include "lwip/netif.h"
#include "lwip/pbuf.h"
#include "lwip/etharp.h"
#include "lwip/stats.h"
#include "netif/ethernet.h"

#define VT_FRAME_BUFFER (VIRTIO_NET_MTU + 64u)

/* Single virtio-net device for this guest. Owned here; vtp_agent.c and the
 * boot code initialize/poll it through the exported accessor. */
virtio_net_t g_vtdev;

static struct netif g_netif;

static err_t vt_low_level_output(struct netif *netif, struct pbuf *p)
{
    static uint8_t frame[VT_FRAME_BUFFER];
    virtio_net_t *dev = (virtio_net_t *)netif->state;
    struct pbuf *q;
    uint16_t off = 0;
    int rc;

    for (q = p; q != NULL; q = q->next) {
        if (off + q->len > (u16_t)sizeof(frame)) {
            return ERR_MEM;
        }
        memcpy(frame + off, q->payload, q->len);
        off = (uint16_t)(off + q->len);
    }
    rc = virtio_net_send(dev, frame, off);
    if (rc != VIRTIO_NET_OK) {
        LINK_STATS_INC(link.memerr);
        return ERR_WOULDBLOCK;
    }
    LINK_STATS_INC(link.xmit);
    return ERR_OK;
}

static void vt_low_level_input(struct netif *netif)
{
    virtio_net_t *dev = (virtio_net_t *)netif->state;
    uint8_t frame[VT_FRAME_BUFFER];
    int len;

    while ((len = virtio_net_recv(dev, frame, sizeof(frame))) > 0) {
        struct pbuf *p = pbuf_alloc(PBUF_RAW, (u16_t)len, PBUF_RAM);
        if (p != NULL) {
            if (pbuf_take(p, frame, (u16_t)len) == ERR_OK &&
                netif->input(p, netif) == ERR_OK) {
                LINK_STATS_INC(link.recv);
            } else {
                pbuf_free(p);
            }
        }
    }
}

static void vt_netif_init(struct netif *netif)
{
    virtio_net_t *dev = (virtio_net_t *)netif->state;

    netif->name[0] = 'v';
    netif->name[1] = 't';
    netif->output = etharp_output;
    netif->linkoutput = vt_low_level_output;
    netif->mtu = VIRTIO_NET_MTU;
    netif->hwaddr_len = 6;
    memcpy(netif->hwaddr, virtio_net_mac(dev), 6);
    netif->flags = NETIF_FLAG_BROADCAST | NETIF_FLAG_ETHARP | NETIF_FLAG_ETHERNET |
                   NETIF_FLAG_IGMP;

    if (virtio_net_link_status(dev) != 0) {
        netif_set_link_up(netif);
    }
    netif_set_up(netif);
}

/* Add the netif with a fixed 10.0.2.16/24 address. */
void vt_netif_add(void)
{
    ip4_addr_t ip, mask, gw;

    IP4_ADDR(&ip, 10, 0, 2, 16);
    IP4_ADDR(&mask, 255, 255, 255, 0);
    IP4_ADDR(&gw, 0, 0, 0, 0);

    netif_add(&g_netif, &ip, &mask, &gw, (void *)&g_vtdev, vt_netif_init,
              ethernet_input);
    netif_set_default(&g_netif);
}

/* Accessor for the device instance (boot code and the agent use it). */
virtio_net_t *vt_netif_dev(void)
{
    return &g_vtdev;
}

/* Drain the virtio RX queue into lwIP. Call from a periodic task. */
void vt_netif_poll(void)
{
    if (netif_is_up(&g_netif)) {
        vt_low_level_input(&g_netif);
    }
}

struct netif *vt_netif_get(void)
{
    return &g_netif;
}
