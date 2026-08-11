/* SPDX-License-Identifier: Apache-2.0 */
#include "../common/include/ivc_sdk.h"

#include <assert.h>
#include <stdio.h>
#include <string.h>

#define BAR2_SIZE (2U * 1024U * 1024U)
#define IMAGE_BYTES (512U * 1024U)
#define PENDING_CAPACITY 16U

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
    static uint8_t bar2[BAR2_SIZE];
    static uint8_t tx_image[IMAGE_BYTES];
    static uint8_t rx_image[IMAGE_BYTES];
    struct ivc_pending_entry zephyr_pending[PENDING_CAPACITY];
    struct ivc_pending_entry linux_pending[PENDING_CAPACITY];
    struct ivc_sdk zephyr;
    struct ivc_sdk linux;
    struct ivc_sdk_image image;
    struct ivc_sdk_received_image received_image;
    struct ivc_sdk_control control;
    struct ivc_sdk_control_view received_control;
    struct ivc_sdk_control_result_view result;
    struct ivc_pending_entry completed;
    struct ivc_message msg;
    uint8_t payload[sizeof(struct ivc_image_desc) + IVC_SDK_CONTROL_ARGS_MAX];
    uint64_t image_seq = 0;
    uint64_t control_seq = 0;

    for (uint32_t i = 0; i < IMAGE_BYTES; i++) {
        tx_image[i] = (uint8_t)(i * 7U + 3U);
    }

    assert(ivc_sdk_shared_init(bar2, sizeof(bar2), 8192, 8192) == IVC_OK);
    assert(ivc_sdk_init(&zephyr, bar2, sizeof(bar2), IVC_PEER_ZEPHYR,
                        noop_doorbell, NULL, NULL, NULL) == IVC_OK);
    assert(ivc_sdk_init(&linux, bar2, sizeof(bar2), IVC_PEER_LINUX,
                        noop_doorbell, NULL, NULL, NULL) == IVC_OK);
    ivc_sdk_set_pending_table(&zephyr, zephyr_pending, PENDING_CAPACITY);
    ivc_sdk_set_pending_table(&linux, linux_pending, PENDING_CAPACITY);

    image.image_id = 42;
    image.width = 1024;
    image.height = 512;
    image.pixel_format = IVC_PIXEL_FORMAT_GRAY8;
    image.data = tx_image;
    image.data_len = sizeof(tx_image);
    assert(ivc_sdk_send_image(&zephyr, &image, 1000, 1000, &image_seq) ==
           IVC_OK);
    printf("Zephyr SDK sends image seq=%llu image_id=%llu\n",
           (unsigned long long)image_seq,
           (unsigned long long)image.image_id);

    prepare_message(&msg, payload, sizeof(payload));
    assert(ivc_sdk_recv(&linux, &msg, IVC_RECV_NO_WAIT) == IVC_OK);
    assert(ivc_sdk_recv_image(&msg, &received_image) == IVC_OK);
    assert(received_image.image_id == image.image_id);
    assert(received_image.data_len == sizeof(tx_image));
    assert(ivc_sdk_read_image(&linux, &received_image, rx_image,
                              sizeof(rx_image)) == IVC_OK);
    assert(memcmp(tx_image, rx_image, sizeof(tx_image)) == 0);
    assert(ivc_sdk_release_image(&linux, &received_image) == IVC_OK);
    printf("Linux SDK receives image seq=%llu image_id=%llu bytes=%u\n",
           (unsigned long long)msg.header.seq,
           (unsigned long long)received_image.image_id,
           received_image.data_len);

    control.command = IVC_CMD_SET_EXPOSURE;
    control.flags = 0;
    control.target_id = received_image.image_id;
    control.args = "apply";
    control.arg_len = 6;
    assert(ivc_sdk_send_control(&linux, &control, msg.header.seq, 2000,
                                1000, &control_seq) == IVC_OK);
    printf("Linux SDK sends control seq=%llu reply_to=%llu\n",
           (unsigned long long)control_seq,
           (unsigned long long)msg.header.seq);

    prepare_message(&msg, payload, sizeof(payload));
    assert(ivc_sdk_recv(&zephyr, &msg, IVC_RECV_NO_WAIT) == IVC_OK);
    assert(ivc_sdk_recv_control(&msg, &received_control) == IVC_OK);
    assert(ivc_sdk_complete_reply(&zephyr, &msg, &completed) == IVC_OK);
    assert(completed.user_data == image.image_id);
    assert(received_control.command == IVC_CMD_SET_EXPOSURE);
    assert(received_control.target_id == image.image_id);
    printf("Zephyr SDK receives control seq=%llu reply_to=%llu\n",
           (unsigned long long)msg.header.seq,
           (unsigned long long)msg.header.reply_to);

    assert(ivc_sdk_reply_control_result(
               &zephyr, &msg, received_control.command, IVC_CONTROL_OK,
               received_control.target_id, NULL) == IVC_OK);

    prepare_message(&msg, payload, sizeof(payload));
    assert(ivc_sdk_recv(&linux, &msg, IVC_RECV_NO_WAIT) == IVC_OK);
    assert(ivc_sdk_complete_reply(&linux, &msg, &completed) == IVC_OK);
    assert(completed.user_data == image.image_id);
    assert(ivc_sdk_recv_control_result(&msg, &result) == IVC_OK);
    assert(result.command == IVC_CMD_SET_EXPOSURE);
    assert(result.status == IVC_CONTROL_OK);
    assert(result.target_id == image.image_id);
    printf("Linux SDK receives result reply_to=%llu status=%d\n",
           (unsigned long long)msg.header.reply_to, result.status);

    image.image_id = 84;
    image.width = 640;
    image.height = 512;
    image.pixel_format = IVC_PIXEL_FORMAT_GRAY8;
    image.data = tx_image;
    image.data_len = 256U * 1024U;
    assert(ivc_sdk_send_image(&linux, &image, 3000, 1000, &image_seq) ==
           IVC_OK);
    printf("Linux SDK sends image seq=%llu image_id=%llu\n",
           (unsigned long long)image_seq,
           (unsigned long long)image.image_id);

    prepare_message(&msg, payload, sizeof(payload));
    assert(ivc_sdk_recv(&zephyr, &msg, IVC_RECV_NO_WAIT) == IVC_OK);
    assert(ivc_sdk_recv_image(&msg, &received_image) == IVC_OK);
    assert(received_image.image_id == image.image_id);
    assert(received_image.data_len == image.data_len);
    memset(rx_image, 0, sizeof(rx_image));
    assert(ivc_sdk_read_image(&zephyr, &received_image, rx_image,
                              sizeof(rx_image)) == IVC_OK);
    assert(memcmp(tx_image, rx_image, image.data_len) == 0);
    assert(ivc_sdk_release_image(&zephyr, &received_image) == IVC_OK);
    printf("Zephyr SDK receives image seq=%llu image_id=%llu bytes=%u\n",
           (unsigned long long)msg.header.seq,
           (unsigned long long)received_image.image_id,
           received_image.data_len);

    control.command = IVC_CMD_CAPTURE_ONCE;
    control.flags = 0;
    control.target_id = received_image.image_id;
    control.args = NULL;
    control.arg_len = 0;
    assert(ivc_sdk_send_control(&zephyr, &control, msg.header.seq, 4000,
                                1000, &control_seq) == IVC_OK);
    printf("Zephyr SDK sends control seq=%llu reply_to=%llu\n",
           (unsigned long long)control_seq,
           (unsigned long long)msg.header.seq);

    prepare_message(&msg, payload, sizeof(payload));
    assert(ivc_sdk_recv(&linux, &msg, IVC_RECV_NO_WAIT) == IVC_OK);
    assert(ivc_sdk_recv_control(&msg, &received_control) == IVC_OK);
    assert(ivc_sdk_complete_reply(&linux, &msg, &completed) == IVC_OK);
    assert(completed.user_data == image.image_id);
    assert(received_control.command == IVC_CMD_CAPTURE_ONCE);
    assert(received_control.target_id == image.image_id);
    printf("Linux SDK receives control seq=%llu reply_to=%llu\n",
           (unsigned long long)msg.header.seq,
           (unsigned long long)msg.header.reply_to);

    assert(ivc_sdk_reply_control_result(
               &linux, &msg, received_control.command, IVC_CONTROL_OK,
               received_control.target_id, NULL) == IVC_OK);

    prepare_message(&msg, payload, sizeof(payload));
    assert(ivc_sdk_recv(&zephyr, &msg, IVC_RECV_NO_WAIT) == IVC_OK);
    assert(ivc_sdk_complete_reply(&zephyr, &msg, &completed) == IVC_OK);
    assert(completed.user_data == image.image_id);
    assert(ivc_sdk_recv_control_result(&msg, &result) == IVC_OK);
    assert(result.command == IVC_CMD_CAPTURE_ONCE);
    assert(result.status == IVC_CONTROL_OK);
    assert(result.target_id == image.image_id);
    printf("Zephyr SDK receives result reply_to=%llu status=%d\n",
           (unsigned long long)msg.header.reply_to, result.status);

    puts("ivc sdk smoke pass");
    return 0;
}
