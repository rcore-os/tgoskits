/* SPDX-License-Identifier: Apache-2.0 */
#include "ivc_client.h"

#include <string.h>

int ivc_client_init(struct ivc_client *client, void *bar2, uint32_t total_size,
                    enum ivc_peer peer, ivc_doorbell_fn doorbell,
                    void *doorbell_ctx, ivc_wait_fn wait, void *wait_ctx)
{
    int rc;

    if (client == NULL) {
        return IVC_ERR_INVALID_ARG;
    }

    memset(client, 0, sizeof(*client));
    rc = ivc_endpoint_bind(&client->endpoint, bar2, total_size, peer, doorbell,
                           doorbell_ctx);
    if (rc != IVC_OK) {
        return rc;
    }
    client->wait = wait;
    client->wait_ctx = wait_ctx;
    return IVC_OK;
}

void ivc_client_set_pending_table(struct ivc_client *client,
                                  struct ivc_pending_entry *entries,
                                  uint32_t capacity)
{
    if (client == NULL) {
        return;
    }
    client->pending.entries = entries;
    client->pending.capacity = capacity;
    if (entries != NULL && capacity != 0U) {
        memset(entries, 0, sizeof(entries[0]) * capacity);
    }
}

static int ivc_pending_track(struct ivc_client *client, uint64_t seq,
                             uint64_t user_data, uint64_t now_ms,
                             uint32_t timeout_ms)
{
    struct ivc_pending_entry *free_entry = NULL;

    if (client == NULL || client->pending.entries == NULL ||
        client->pending.capacity == 0U) {
        return IVC_ERR_INVALID_ARG;
    }

    for (uint32_t i = 0; i < client->pending.capacity; i++) {
        struct ivc_pending_entry *entry = &client->pending.entries[i];
        if (entry->state == IVC_PENDING_WAITING && entry->seq == seq) {
            return IVC_ERR_CORRUPT;
        }
        if (free_entry == NULL && entry->state != IVC_PENDING_WAITING) {
            free_entry = entry;
        }
    }

    if (free_entry == NULL) {
        return IVC_ERR_NO_SPACE;
    }

    free_entry->seq = seq;
    free_entry->user_data = user_data;
    free_entry->created_at_ms = now_ms;
    free_entry->timeout_ms = timeout_ms;
    free_entry->state = IVC_PENDING_WAITING;
    return IVC_OK;
}

int ivc_client_send(struct ivc_client *client, uint32_t type,
                    const void *payload, uint32_t payload_len,
                    uint64_t *seq_out)
{
    if (client == NULL) {
        return IVC_ERR_INVALID_ARG;
    }
    return ivc_send(&client->endpoint, type, 0, 0, payload, payload_len,
                    seq_out);
}

int ivc_client_send_request(struct ivc_client *client, uint32_t type,
                            const void *payload, uint32_t payload_len,
                            uint64_t user_data, uint64_t now_ms,
                            uint32_t timeout_ms, uint64_t *seq_out)
{
    return ivc_client_send_request_to(client, 0, type, payload, payload_len,
                                      user_data, now_ms, timeout_ms, seq_out);
}

int ivc_client_send_request_to(struct ivc_client *client, uint64_t reply_to,
                               uint32_t type, const void *payload,
                               uint32_t payload_len, uint64_t user_data,
                               uint64_t now_ms, uint32_t timeout_ms,
                               uint64_t *seq_out)
{
    uint64_t seq;
    int rc;
    uint32_t flags = IVC_MSG_F_NEEDS_REPLY;

    if (client == NULL) {
        return IVC_ERR_INVALID_ARG;
    }
    if (reply_to != 0U) {
        flags |= IVC_MSG_F_IS_REPLY;
    }

    seq = client->endpoint.next_seq;
    rc = ivc_pending_track(client, seq, user_data, now_ms, timeout_ms);
    if (rc != IVC_OK) {
        return rc;
    }

    rc = ivc_send(&client->endpoint, type, flags, reply_to, payload,
                  payload_len, &seq);
    if (rc != IVC_OK) {
        for (uint32_t i = 0; i < client->pending.capacity; i++) {
            struct ivc_pending_entry *entry = &client->pending.entries[i];
            if (entry->state == IVC_PENDING_WAITING && entry->seq == seq) {
                entry->state = IVC_PENDING_UNUSED;
                break;
            }
        }
        return rc;
    }
    if (seq_out != NULL) {
        *seq_out = seq;
    }
    return IVC_OK;
}

int ivc_client_reply(struct ivc_client *client,
                     const struct ivc_message *request, uint32_t type,
                     const void *payload, uint32_t payload_len,
                     uint64_t *seq_out)
{
    if (client == NULL || request == NULL) {
        return IVC_ERR_INVALID_ARG;
    }
    return ivc_client_reply_to(client, request->header.seq, type, payload,
                               payload_len, seq_out);
}

int ivc_client_reply_to(struct ivc_client *client, uint64_t reply_to,
                        uint32_t type, const void *payload,
                        uint32_t payload_len, uint64_t *seq_out)
{
    if (client == NULL) {
        return IVC_ERR_INVALID_ARG;
    }
    return ivc_send(&client->endpoint, type, IVC_MSG_F_IS_REPLY,
                    reply_to, payload, payload_len, seq_out);
}

int ivc_client_poll(struct ivc_client *client, struct ivc_message *msg)
{
    if (client == NULL || msg == NULL) {
        return IVC_ERR_INVALID_ARG;
    }
    return ivc_poll_recv(&client->endpoint, &msg->header, msg->payload,
                         msg->payload_capacity, &msg->payload_len);
}

int ivc_client_recv(struct ivc_client *client, struct ivc_message *msg,
                    int timeout_ms)
{
    int rc;

    if (client == NULL || msg == NULL) {
        return IVC_ERR_INVALID_ARG;
    }

    for (;;) {
        rc = ivc_client_poll(client, msg);
        if (rc != IVC_ERR_EMPTY) {
            return rc;
        }
        if (timeout_ms == IVC_RECV_NO_WAIT || client->wait == NULL) {
            return IVC_ERR_EMPTY;
        }
        rc = client->wait(client->wait_ctx, timeout_ms);
        if (rc != IVC_OK) {
            return rc;
        }
        if (timeout_ms != IVC_RECV_WAIT_FOREVER) {
            timeout_ms = IVC_RECV_NO_WAIT;
        }
    }
}

int ivc_client_pending_lookup(const struct ivc_client *client, uint64_t seq,
                              struct ivc_pending_entry *entry_out)
{
    if (client == NULL || client->pending.entries == NULL) {
        return IVC_ERR_INVALID_ARG;
    }

    for (uint32_t i = 0; i < client->pending.capacity; i++) {
        const struct ivc_pending_entry *entry = &client->pending.entries[i];
        if (entry->state == IVC_PENDING_WAITING && entry->seq == seq) {
            if (entry_out != NULL) {
                *entry_out = *entry;
            }
            return IVC_OK;
        }
    }
    return IVC_ERR_NOT_FOUND;
}

int ivc_client_complete_reply(struct ivc_client *client,
                              const struct ivc_message *reply,
                              struct ivc_pending_entry *entry_out)
{
    if (client == NULL || reply == NULL || client->pending.entries == NULL) {
        return IVC_ERR_INVALID_ARG;
    }
    if ((reply->header.flags & IVC_MSG_F_IS_REPLY) == 0U ||
        reply->header.reply_to == 0U) {
        return IVC_ERR_INVALID_ARG;
    }

    for (uint32_t i = 0; i < client->pending.capacity; i++) {
        struct ivc_pending_entry *entry = &client->pending.entries[i];
        if (entry->state == IVC_PENDING_WAITING &&
            entry->seq == reply->header.reply_to) {
            if (entry_out != NULL) {
                *entry_out = *entry;
            }
            entry->state = IVC_PENDING_COMPLETED;
            return IVC_OK;
        }
    }
    return IVC_ERR_NOT_FOUND;
}

int ivc_client_expire_pending(struct ivc_client *client, uint64_t now_ms,
                              uint32_t *expired_out)
{
    uint32_t expired = 0;

    if (client == NULL || client->pending.entries == NULL) {
        return IVC_ERR_INVALID_ARG;
    }

    for (uint32_t i = 0; i < client->pending.capacity; i++) {
        struct ivc_pending_entry *entry = &client->pending.entries[i];
        if (entry->state != IVC_PENDING_WAITING ||
            entry->timeout_ms == 0U) {
            continue;
        }
        if (now_ms - entry->created_at_ms >= entry->timeout_ms) {
            entry->state = IVC_PENDING_TIMED_OUT;
            expired++;
        }
    }

    if (expired_out != NULL) {
        *expired_out = expired;
    }
    return expired == 0U ? IVC_ERR_NOT_FOUND : IVC_ERR_TIMEOUT;
}
