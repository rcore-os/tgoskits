/* SPDX-License-Identifier: Apache-2.0 */
#include "../common/include/ivc_demo.h"

#include <assert.h>
#include <stdio.h>
#include <string.h>

#define DEMO_ROUNDS 100U
#define PENDING_CAPACITY 128U
#define IMAGE_BYTES 32U

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

static uint32_t command_for_round(uint32_t round)
{
    switch (round % 4U) {
    case 0:
        return IVC_CMD_SET_EXPOSURE;
    case 1:
        return IVC_CMD_SET_GAIN;
    case 2:
        return IVC_CMD_CAPTURE_ONCE;
    default:
        return IVC_CMD_STOP_CAPTURE;
    }
}

int main(void)
{
    static uint8_t bar2[65536];
    struct ivc_pending_entry zephyr_pending[PENDING_CAPACITY];
    struct ivc_pending_entry linux_pending[PENDING_CAPACITY];
    struct ivc_client zephyr;
    struct ivc_client linux;
    uint32_t done_count = 0;
    uint32_t failed_count = 0;

    assert(ivc_shared_init(bar2, sizeof(bar2), 24576, 24576) == IVC_OK);
    assert(ivc_client_init(&zephyr, bar2, sizeof(bar2), IVC_PEER_ZEPHYR,
                           noop_doorbell, NULL, NULL, NULL) == IVC_OK);
    assert(ivc_client_init(&linux, bar2, sizeof(bar2), IVC_PEER_LINUX,
                           noop_doorbell, NULL, NULL, NULL) == IVC_OK);
    ivc_client_set_pending_table(&zephyr, zephyr_pending, PENDING_CAPACITY);
    ivc_client_set_pending_table(&linux, linux_pending, PENDING_CAPACITY);

    for (uint32_t round = 0; round < DEMO_ROUNDS; round++) {
        uint8_t image_bytes[IMAGE_BYTES];
        uint8_t image_payload[sizeof(struct ivc_image_frame) + IMAGE_BYTES];
        uint8_t control_payload[sizeof(struct ivc_control_cmd) + 16U];
        struct ivc_control_result result;
        struct ivc_message msg;
        const struct ivc_image_frame *image;
        const struct ivc_control_cmd *cmd;
        struct ivc_pending_entry completed;
        uint64_t image_id = 100U + round;
        uint64_t image_seq = 0;
        uint64_t command_seq = 0;
        uint64_t result_seq = 0;
        uint32_t image_payload_len = 0;
        uint32_t control_payload_len = 0;
        uint32_t command = round == DEMO_ROUNDS - 1U ? 0xffff0000U :
                                                       command_for_round(round);
        uint32_t reply_type = 0;

        for (uint32_t i = 0; i < IMAGE_BYTES; i++) {
            image_bytes[i] = (uint8_t)(round + i);
        }

        assert(ivc_demo_make_image_frame(
                   image_payload, sizeof(image_payload), image_id, 640, 480,
                   IVC_PIXEL_FORMAT_GRAY8, image_bytes, sizeof(image_bytes),
                   &image_payload_len) == IVC_OK);
        assert(ivc_client_send_request(&zephyr, IVC_MSG_IMAGE_FRAME,
                                       image_payload, image_payload_len,
                                       image_id, round, 1000U,
                                       &image_seq) == IVC_OK);
        printf("Zephyr sends IMAGE_FRAME seq=%llu image_id=%llu\n",
               (unsigned long long)image_seq, (unsigned long long)image_id);

        prepare_message(&msg, image_payload, sizeof(image_payload));
        assert(ivc_client_recv(&linux, &msg, IVC_RECV_NO_WAIT) == IVC_OK);
        assert(ivc_demo_parse_image_frame(&msg, &image) == IVC_OK);
        assert(image->image_id == image_id);
        printf("Linux receives IMAGE_FRAME seq=%llu image_id=%llu\n",
               (unsigned long long)msg.header.seq,
               (unsigned long long)image->image_id);

        assert(ivc_demo_make_control_cmd(
                   control_payload, sizeof(control_payload), command, 0,
                   image->image_id, "apply", 6, &control_payload_len) ==
               IVC_OK);
        assert(ivc_client_send_request_to(
                   &linux, msg.header.seq, IVC_MSG_CONTROL_CMD,
                   control_payload, control_payload_len, msg.header.seq,
                   2000U + round, 1000U, &command_seq) == IVC_OK);
        printf("Linux sends CONTROL_CMD seq=%llu reply_to=%llu\n",
               (unsigned long long)command_seq,
               (unsigned long long)msg.header.seq);

        prepare_message(&msg, control_payload, sizeof(control_payload));
        assert(ivc_client_recv(&zephyr, &msg, IVC_RECV_NO_WAIT) == IVC_OK);
        assert(ivc_demo_parse_control_cmd(&msg, &cmd) == IVC_OK);
        assert(ivc_client_complete_reply(&zephyr, &msg, &completed) ==
               IVC_OK);
        assert(completed.user_data == image_id);
        printf("Zephyr receives CONTROL_CMD seq=%llu reply_to=%llu\n",
               (unsigned long long)msg.header.seq,
               (unsigned long long)msg.header.reply_to);

        assert(ivc_demo_execute_control_cmd(cmd, &result, &reply_type) ==
               IVC_OK);
        printf("Zephyr executes CONTROL_CMD immediately\n");
        assert(ivc_client_reply(&zephyr, &msg, reply_type, &result,
                                sizeof(result), &result_seq) == IVC_OK);
        if (reply_type == IVC_MSG_CONTROL_DONE) {
            done_count++;
            printf("Zephyr sends CONTROL_DONE seq=%llu reply_to=%llu\n",
                   (unsigned long long)result_seq,
                   (unsigned long long)msg.header.seq);
        } else {
            failed_count++;
            printf("Zephyr sends CONTROL_FAILED seq=%llu reply_to=%llu "
                   "reason=\"%s\"\n",
                   (unsigned long long)result_seq,
                   (unsigned long long)msg.header.seq,
                   ivc_demo_control_status_string(result.status));
        }

        prepare_message(&msg, &result, sizeof(result));
        assert(ivc_client_recv(&linux, &msg, IVC_RECV_NO_WAIT) == IVC_OK);
        assert(ivc_client_complete_reply(&linux, &msg, &completed) == IVC_OK);
        assert(completed.user_data == image_seq);
        if (msg.header.msg_type == IVC_MSG_CONTROL_DONE) {
            printf("Linux receives CONTROL_DONE reply_to=%llu\n",
                   (unsigned long long)msg.header.reply_to);
        } else {
            printf("Linux receives CONTROL_FAILED reply_to=%llu reason=\"%s\"\n",
                   (unsigned long long)msg.header.reply_to,
                   ivc_demo_control_status_string(result.status));
        }
    }

    assert(done_count == DEMO_ROUNDS - 1U);
    assert(failed_count == 1U);
    printf("ivc demo flow pass rounds=%u done=%u failed=%u\n", DEMO_ROUNDS,
           done_count, failed_count);
    return 0;
}
