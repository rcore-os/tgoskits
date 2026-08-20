/*
 * VTP agent public API (guests/freertos-vtp).
 */

#ifndef VTP_AGENT_H
#define VTP_AGENT_H

#ifdef __cplusplus
extern "C" {
#endif

/* FreeRTOS task entry. Requires the netif to be added (vt_netif_add) and the
 * virtio-net device initialized. Never returns; deletes its own task. */
void vtp_agent_task(void *arg);

#ifdef __cplusplus
}
#endif

#endif /* VTP_AGENT_H */
