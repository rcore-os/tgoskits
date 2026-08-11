/* SPDX-License-Identifier: Apache-2.0 */
#ifndef AXVISOR_IVSHMEM_IVC_CLIENT_H
#define AXVISOR_IVSHMEM_IVC_CLIENT_H

#include "ivc_ring.h"

#ifdef __cplusplus
extern "C" {
#endif

#define IVC_RECV_NO_WAIT 0
#define IVC_RECV_WAIT_FOREVER (-1)

typedef int (*ivc_wait_fn)(void *ctx, int timeout_ms);

enum ivc_pending_state {
    IVC_PENDING_UNUSED = 0,
    IVC_PENDING_WAITING = 1,
    IVC_PENDING_COMPLETED = 2,
    IVC_PENDING_TIMED_OUT = 3,
};

struct ivc_message {
    struct ivc_msg_header header;
    void *payload;
    uint32_t payload_capacity;
    uint32_t payload_len;
};

struct ivc_pending_entry {
    uint64_t seq;
    uint64_t user_data;
    uint64_t created_at_ms;
    uint32_t timeout_ms;
    uint32_t state;
};

struct ivc_pending_table {
    struct ivc_pending_entry *entries;
    uint32_t capacity;
};

struct ivc_client {
    struct ivc_endpoint endpoint;
    ivc_wait_fn wait;
    void *wait_ctx;
    struct ivc_pending_table pending;
};

int ivc_client_init(struct ivc_client *client, void *bar2, uint32_t total_size,
                    enum ivc_peer peer, ivc_doorbell_fn doorbell,
                    void *doorbell_ctx, ivc_wait_fn wait, void *wait_ctx);
void ivc_client_set_pending_table(struct ivc_client *client,
                                  struct ivc_pending_entry *entries,
                                  uint32_t capacity);
int ivc_client_send(struct ivc_client *client, uint32_t type,
                    const void *payload, uint32_t payload_len,
                    uint64_t *seq_out);
int ivc_client_send_request(struct ivc_client *client, uint32_t type,
                            const void *payload, uint32_t payload_len,
                            uint64_t user_data, uint64_t now_ms,
                            uint32_t timeout_ms, uint64_t *seq_out);
int ivc_client_send_request_to(struct ivc_client *client, uint64_t reply_to,
                               uint32_t type, const void *payload,
                               uint32_t payload_len, uint64_t user_data,
                               uint64_t now_ms, uint32_t timeout_ms,
                               uint64_t *seq_out);
int ivc_client_recv(struct ivc_client *client, struct ivc_message *msg,
                    int timeout_ms);
int ivc_client_reply(struct ivc_client *client,
                     const struct ivc_message *request, uint32_t type,
                     const void *payload, uint32_t payload_len,
                     uint64_t *seq_out);
int ivc_client_reply_to(struct ivc_client *client, uint64_t reply_to,
                        uint32_t type, const void *payload,
                        uint32_t payload_len, uint64_t *seq_out);
int ivc_client_poll(struct ivc_client *client, struct ivc_message *msg);
int ivc_client_pending_lookup(const struct ivc_client *client, uint64_t seq,
                              struct ivc_pending_entry *entry_out);
int ivc_client_complete_reply(struct ivc_client *client,
                              const struct ivc_message *reply,
                              struct ivc_pending_entry *entry_out);
int ivc_client_expire_pending(struct ivc_client *client, uint64_t now_ms,
                              uint32_t *expired_out);

#ifdef __cplusplus
}
#endif

#endif /* AXVISOR_IVSHMEM_IVC_CLIENT_H */
