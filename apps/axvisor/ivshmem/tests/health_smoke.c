/* SPDX-License-Identifier: Apache-2.0 */
#include "../common/include/ivc_demo.h"

#include <assert.h>
#include <stdio.h>
#include <string.h>

#define PENDING_CAPACITY 8U

static void noop_doorbell(void *ctx)
{
    (void)ctx;
}

static void prepare_message(struct ivc_message *msg, void *payload,
                            uint32_t capacity)
{
    memset(msg, 0, sizeof(*msg));
    if (payload != NULL && capacity != 0U) {
        memset(payload, 0, capacity);
    }
    msg->payload = payload;
    msg->payload_capacity = capacity;
}

int main(void)
{
    static uint8_t bar2[8192];
    struct ivc_pending_entry linux_pending[PENDING_CAPACITY];
    struct ivc_client zephyr;
    struct ivc_client linux;
    struct ivc_message msg;
    uint8_t payload[128];
    uint32_t payload_len = 0;
    uint32_t expired = 0;
    uint64_t timeout_seq = 0;
    const struct ivc_error_payload *error;
    const struct ivc_heartbeat_payload *heartbeat;

    assert(ivc_shared_init(bar2, sizeof(bar2), 2048, 2048) == IVC_OK);
    assert(ivc_client_init(&zephyr, bar2, sizeof(bar2), IVC_PEER_ZEPHYR,
                           noop_doorbell, NULL, NULL, NULL) == IVC_OK);
    assert(ivc_client_init(&linux, bar2, sizeof(bar2), IVC_PEER_LINUX,
                           noop_doorbell, NULL, NULL, NULL) == IVC_OK);
    ivc_client_set_pending_table(&linux, linux_pending, PENDING_CAPACITY);

    assert(ivc_client_send_request(&linux, IVC_MSG_CONTROL_CMD, "wait", 5,
                                   0xdeadU, 100U, 10U,
                                   &timeout_seq) == IVC_OK);
    assert(timeout_seq == 1U);
    assert(ivc_client_expire_pending(&linux, 109U, &expired) ==
           IVC_ERR_NOT_FOUND);
    assert(expired == 0U);
    assert(ivc_client_expire_pending(&linux, 110U, &expired) ==
           IVC_ERR_TIMEOUT);
    assert(expired == 1U);
    printf("Linux pending timeout seq=%llu rc=%d\n",
           (unsigned long long)timeout_seq, IVC_ERR_TIMEOUT);
    prepare_message(&msg, payload, sizeof(payload));
    assert(ivc_client_recv(&zephyr, &msg, IVC_RECV_NO_WAIT) == IVC_OK);
    assert(msg.header.msg_type == IVC_MSG_CONTROL_CMD);

    assert(ivc_demo_make_control_cmd(payload, sizeof(payload), 0xffff0000U, 0,
                                     7U, NULL, 0, &payload_len) == IVC_OK);
    assert(ivc_client_send(&linux, IVC_MSG_CONTROL_CMD, payload, payload_len,
                           NULL) == IVC_OK);
    prepare_message(&msg, payload, sizeof(payload));
    assert(ivc_client_recv(&zephyr, &msg, IVC_RECV_NO_WAIT) == IVC_OK);
    assert(ivc_demo_make_error(payload, sizeof(payload),
                               IVC_PROTO_ERR_UNKNOWN_COMMAND,
                               "unknown command", &payload_len) == IVC_OK);
    assert(ivc_client_reply(&zephyr, &msg, IVC_MSG_CONTROL_FAILED, payload,
                            payload_len, NULL) == IVC_OK);

    prepare_message(&msg, payload, sizeof(payload));
    assert(ivc_client_recv(&linux, &msg, IVC_RECV_NO_WAIT) == IVC_OK);
    assert(msg.header.msg_type == IVC_MSG_CONTROL_FAILED);
    assert(ivc_demo_parse_error(&msg, &error) == IVC_OK);
    assert(error->code == IVC_PROTO_ERR_UNKNOWN_COMMAND);
    assert(error->detail_len == strlen("unknown command"));
    printf("Zephyr CONTROL_FAILED code=%u reason=\"%.*s\"\n", error->code,
           (int)error->detail_len, error->detail);

    assert(ivc_demo_make_heartbeat(payload, sizeof(payload), 0xabcU, 1234U,
                                   &payload_len) == IVC_OK);
    assert(ivc_client_send(&zephyr, IVC_MSG_HEARTBEAT, payload, payload_len,
                           NULL) == IVC_OK);
    prepare_message(&msg, payload, sizeof(payload));
    assert(ivc_client_recv(&linux, &msg, IVC_RECV_NO_WAIT) == IVC_OK);
    assert(ivc_demo_parse_heartbeat(&msg, &heartbeat) == IVC_OK);
    assert(heartbeat->nonce == 0xabcU);
    assert(heartbeat->uptime_ms == 1234U);
    printf("Linux receives HEARTBEAT nonce=%llu uptime_ms=%llu\n",
           (unsigned long long)heartbeat->nonce,
           (unsigned long long)heartbeat->uptime_ms);

    assert(ivc_demo_make_heartbeat(payload, sizeof(payload), 0xdefU, 5678U,
                                   &payload_len) == IVC_OK);
    assert(ivc_client_send(&linux, IVC_MSG_HEARTBEAT, payload, payload_len,
                           NULL) == IVC_OK);
    prepare_message(&msg, payload, sizeof(payload));
    assert(ivc_client_recv(&zephyr, &msg, IVC_RECV_NO_WAIT) == IVC_OK);
    assert(ivc_demo_parse_heartbeat(&msg, &heartbeat) == IVC_OK);
    assert(heartbeat->nonce == 0xdefU);
    assert(heartbeat->uptime_ms == 5678U);
    printf("Zephyr receives HEARTBEAT nonce=%llu uptime_ms=%llu\n",
           (unsigned long long)heartbeat->nonce,
           (unsigned long long)heartbeat->uptime_ms);

    puts("ivc health smoke pass");
    return 0;
}
