/* SPDX-License-Identifier: Apache-2.0 */
#include "../common/include/ivc_client.h"

#include <assert.h>
#include <stdio.h>
#include <string.h>

#define MESSAGE_COUNT 10U
#define PENDING_CAPACITY 16U

struct image_payload {
    uint64_t image_id;
    char tag[8];
};

static void noop_doorbell(void *ctx)
{
    (void)ctx;
}

static void prepare_message(struct ivc_message *msg, void *payload,
                            uint32_t capacity)
{
    memset(msg, 0, sizeof(*msg));
    memset(payload, 0, capacity);
    msg->payload = payload;
    msg->payload_capacity = capacity;
}

int main(void)
{
    static uint8_t bar2[8192];
    struct ivc_pending_entry zephyr_pending[PENDING_CAPACITY];
    struct ivc_pending_entry linux_pending[PENDING_CAPACITY];
    struct ivc_client zephyr;
    struct ivc_client linux;
    uint64_t image_seq[MESSAGE_COUNT];
    uint64_t command_seq[MESSAGE_COUNT];

    assert(ivc_shared_init(bar2, sizeof(bar2), 2048, 2048) == IVC_OK);
    assert(ivc_client_init(&zephyr, bar2, sizeof(bar2), IVC_PEER_ZEPHYR,
                           noop_doorbell, NULL, NULL, NULL) == IVC_OK);
    assert(ivc_client_init(&linux, bar2, sizeof(bar2), IVC_PEER_LINUX,
                           noop_doorbell, NULL, NULL, NULL) == IVC_OK);
    ivc_client_set_pending_table(&zephyr, zephyr_pending, PENDING_CAPACITY);
    ivc_client_set_pending_table(&linux, linux_pending, PENDING_CAPACITY);

    for (uint32_t i = 0; i < MESSAGE_COUNT; i++) {
        struct image_payload image = {
            .image_id = 1000U + i,
        };
        snprintf(image.tag, sizeof(image.tag), "img%u", i);
        assert(ivc_client_send_request(&zephyr, IVC_MSG_IMAGE_FRAME, &image,
                                       sizeof(image), image.image_id, i * 10U,
                                       1000U, &image_seq[i]) == IVC_OK);
    }

    for (uint32_t i = 0; i < MESSAGE_COUNT; i++) {
        struct ivc_message msg;
        struct image_payload image;

        prepare_message(&msg, &image, sizeof(image));
        assert(ivc_client_recv(&linux, &msg, IVC_RECV_NO_WAIT) == IVC_OK);
        assert(msg.header.msg_type == IVC_MSG_IMAGE_FRAME);
        assert(msg.header.seq == image_seq[i]);
        assert(image.image_id == 1000U + i);
    }

    for (uint32_t i = 0; i < MESSAGE_COUNT; i++) {
        uint32_t reverse = MESSAGE_COUNT - 1U - i;
        char command[16];

        snprintf(command, sizeof(command), "cmd%u", reverse);
        assert(ivc_client_send_request_to(&linux, image_seq[reverse],
                                          IVC_MSG_CONTROL_CMD, command,
                                          (uint32_t)strlen(command) + 1U,
                                          image_seq[reverse], 2000U + i,
                                          1000U,
                                          &command_seq[reverse]) == IVC_OK);
        assert(ivc_client_pending_lookup(&zephyr, image_seq[reverse],
                                         NULL) == IVC_OK);
    }

    for (uint32_t i = 0; i < MESSAGE_COUNT; i++) {
        uint32_t reverse = MESSAGE_COUNT - 1U - i;
        struct ivc_message msg;
        struct ivc_pending_entry completed_image;
        char command[16];

        prepare_message(&msg, command, sizeof(command));
        assert(ivc_client_recv(&zephyr, &msg, IVC_RECV_NO_WAIT) == IVC_OK);
        assert(msg.header.msg_type == IVC_MSG_CONTROL_CMD);
        assert(msg.header.reply_to == image_seq[reverse]);
        assert(ivc_client_complete_reply(&zephyr, &msg,
                                         &completed_image) == IVC_OK);
        assert(completed_image.user_data == 1000U + reverse);

        assert(ivc_client_reply(&zephyr, &msg, IVC_MSG_CONTROL_DONE, NULL, 0,
                                NULL) == IVC_OK);
    }

    for (uint32_t i = 0; i < MESSAGE_COUNT; i++) {
        struct ivc_message msg;
        struct ivc_pending_entry completed_command;

        prepare_message(&msg, NULL, 0);
        assert(ivc_client_recv(&linux, &msg, IVC_RECV_NO_WAIT) == IVC_OK);
        assert(msg.header.msg_type == IVC_MSG_CONTROL_DONE);
        assert(ivc_client_complete_reply(&linux, &msg,
                                         &completed_command) == IVC_OK);
    }

    assert(ivc_client_expire_pending(&zephyr, 5000U, NULL) ==
           IVC_ERR_NOT_FOUND);

    puts("ivc pending smoke pass");
    return 0;
}
