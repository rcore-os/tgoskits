/* SPDX-License-Identifier: Apache-2.0 */
#include "../common/include/ivc_client.h"

#include <assert.h>
#include <stdio.h>
#include <string.h>

struct event_counter {
    unsigned doorbells;
    unsigned waits;
};

static void count_doorbell(void *ctx)
{
    struct event_counter *counter = (struct event_counter *)ctx;
    counter->doorbells++;
}

static int count_wait(void *ctx, int timeout_ms)
{
    struct event_counter *counter = (struct event_counter *)ctx;
    (void)timeout_ms;
    counter->waits++;
    return IVC_OK;
}

int main(void)
{
    static uint8_t bar2[4096];
    struct event_counter zephyr_events = {0};
    struct event_counter linux_events = {0};
    struct ivc_client zephyr;
    struct ivc_client linux;
    struct ivc_message msg;
    char payload[64];
    uint64_t image_seq = 0;
    uint64_t cmd_seq = 0;

    assert(ivc_shared_init(bar2, sizeof(bar2), 1024, 1024) == IVC_OK);
    assert(ivc_client_init(&zephyr, bar2, sizeof(bar2), IVC_PEER_ZEPHYR,
                           count_doorbell, &zephyr_events, count_wait,
                           &zephyr_events) == IVC_OK);
    assert(ivc_client_init(&linux, bar2, sizeof(bar2), IVC_PEER_LINUX,
                           count_doorbell, &linux_events, count_wait,
                           &linux_events) == IVC_OK);

    memset(payload, 0, sizeof(payload));
    msg.payload = payload;
    msg.payload_capacity = sizeof(payload);
    assert(ivc_client_recv(&linux, &msg, IVC_RECV_NO_WAIT) == IVC_ERR_EMPTY);

    assert(ivc_client_send(&zephyr, IVC_MSG_IMAGE_FRAME, "frame0", 6,
                           &image_seq) == IVC_OK);
    assert(zephyr_events.doorbells == 1);

    memset(payload, 0, sizeof(payload));
    assert(ivc_client_recv(&linux, &msg, IVC_RECV_NO_WAIT) == IVC_OK);
    assert(msg.header.msg_type == IVC_MSG_IMAGE_FRAME);
    assert(msg.header.seq == image_seq);
    assert(msg.payload_len == 6);
    assert(memcmp(payload, "frame0", 6) == 0);

    assert(ivc_client_reply(&linux, &msg, IVC_MSG_CONTROL_CMD, "run", 3,
                            &cmd_seq) == IVC_OK);
    assert(linux_events.doorbells == 1);

    memset(payload, 0, sizeof(payload));
    assert(ivc_client_recv(&zephyr, &msg, IVC_RECV_NO_WAIT) == IVC_OK);
    assert(msg.header.msg_type == IVC_MSG_CONTROL_CMD);
    assert(msg.header.seq == cmd_seq);
    assert(msg.header.reply_to == image_seq);
    assert(msg.payload_len == 3);
    assert(memcmp(payload, "run", 3) == 0);

    assert(ivc_client_reply(&zephyr, &msg, IVC_MSG_CONTROL_DONE, NULL, 0,
                            NULL) == IVC_OK);

    memset(payload, 0, sizeof(payload));
    assert(ivc_client_recv(&linux, &msg, IVC_RECV_NO_WAIT) == IVC_OK);
    assert(msg.header.msg_type == IVC_MSG_CONTROL_DONE);
    assert(msg.header.reply_to == cmd_seq);

    puts("ivc client smoke pass");
    return 0;
}
