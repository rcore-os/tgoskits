/* SPDX-License-Identifier: Apache-2.0 */
#include "../common/include/ivc_ring.h"

#include <assert.h>
#include <stdio.h>
#include <string.h>

struct doorbell_counter {
    unsigned count;
};

static void count_doorbell(void *ctx)
{
    struct doorbell_counter *counter = (struct doorbell_counter *)ctx;
    counter->count++;
}

int main(void)
{
    static uint8_t bar2[4096];
    struct ivc_endpoint zephyr;
    struct ivc_endpoint linux;
    struct doorbell_counter z_doorbell = {0};
    struct doorbell_counter l_doorbell = {0};
    struct ivc_msg_header header;
    char payload[64];
    uint32_t payload_len = 0;
    uint64_t hello_seq = 0;
    uint64_t ack_seq = 0;

    assert(ivc_shared_init(bar2, sizeof(bar2), 1024, 1024) == IVC_OK);
    assert(ivc_endpoint_bind(&zephyr, bar2, sizeof(bar2), IVC_PEER_ZEPHYR,
                             count_doorbell, &z_doorbell) == IVC_OK);
    assert(ivc_endpoint_bind(&linux, bar2, sizeof(bar2), IVC_PEER_LINUX,
                             count_doorbell, &l_doorbell) == IVC_OK);

    printf("ivc protocol version=%u bar2_size=%u z_to_l_offset=%llu "
           "z_to_l_size=%llu l_to_z_offset=%llu l_to_z_size=%llu\n",
           zephyr.shared->version, zephyr.shared->total_size,
           (unsigned long long)zephyr.shared->z_to_l_offset,
           (unsigned long long)zephyr.shared->z_to_l_size,
           (unsigned long long)zephyr.shared->l_to_z_offset,
           (unsigned long long)zephyr.shared->l_to_z_size);

    assert(ivc_poll_recv(&linux, &header, payload, sizeof(payload),
                         &payload_len) == IVC_ERR_EMPTY);

    assert(ivc_send(&zephyr, IVC_MSG_HELLO, IVC_MSG_F_NEEDS_REPLY, 0,
                    "hello", 5, &hello_seq) == IVC_OK);
    assert(z_doorbell.count == 1);

    memset(payload, 0, sizeof(payload));
    assert(ivc_recv(&linux, &header, payload, sizeof(payload),
                    &payload_len) == IVC_OK);
    assert(header.msg_type == IVC_MSG_HELLO);
    assert(header.seq == hello_seq);
    assert(header.reply_to == 0);
    assert(payload_len == 5);
    assert(memcmp(payload, "hello", 5) == 0);

    assert(ivc_send(&linux, IVC_MSG_HELLO_ACK,
                    IVC_MSG_F_IS_REPLY, hello_seq, "ack", 3,
                    &ack_seq) == IVC_OK);
    assert(l_doorbell.count == 1);

    memset(payload, 0, sizeof(payload));
    assert(ivc_recv(&zephyr, &header, payload, sizeof(payload),
                    &payload_len) == IVC_OK);
    assert(header.msg_type == IVC_MSG_HELLO_ACK);
    assert(header.seq == ack_seq);
    assert(header.reply_to == hello_seq);
    assert(payload_len == 3);
    assert(memcmp(payload, "ack", 3) == 0);

    while (ivc_send(&zephyr, IVC_MSG_HEARTBEAT, 0, 0, payload,
                    sizeof(payload), NULL) == IVC_OK) {
    }
    assert(ivc_send(&zephyr, IVC_MSG_HEARTBEAT, 0, 0, payload,
                    sizeof(payload), NULL) == IVC_ERR_NO_SPACE);

    puts("ivc ring smoke pass");
    return 0;
}
