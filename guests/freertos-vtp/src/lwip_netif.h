/*
 * Public API of the lwIP netif glue (guests/freertos-vtp).
 */

#ifndef LWIP_NETIF_H
#define LWIP_NETIF_H

#include "virtio_net.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Add the netif with the fixed 10.0.2.16/24 address. */
void vt_netif_add(void);

/* Drain the virtio RX queue into lwIP; call from a periodic task. */
void vt_netif_poll(void);

/* The driver instance (initialize via virtio_net_init before vt_netif_add). */
virtio_net_t *vt_netif_dev(void);

#ifdef __cplusplus
}
#endif

#endif /* LWIP_NETIF_H */
