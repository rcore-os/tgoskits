/* SPDX-License-Identifier: Apache-2.0 */
#include "../common/include/ivc_demo.h"

#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define PENDING_CAPACITY 8U
#define BAR2_SIZE (16U * 1024U * 1024U)

static void noop_doorbell(void *ctx)
{
    (void)ctx;
}

static void fill_pattern(uint8_t *data, uint32_t len, uint32_t seed)
{
    for (uint32_t i = 0; i < len; i++) {
        data[i] = (uint8_t)(seed + i * 13U);
    }
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
    static const uint32_t sizes[] = {
        256U * 1024U,
        512U * 1024U,
        1024U * 1024U,
        10U * 1024U * 1024U,
    };
    struct ivc_pending_entry zephyr_pending[PENDING_CAPACITY];
    struct ivc_client zephyr;
    struct ivc_client linux;
    uint8_t desc_payload[sizeof(struct ivc_image_desc)];
    uint8_t rx_desc_payload[sizeof(struct ivc_image_desc)];
    uint64_t reusable_offset = 0;

    assert(ivc_shared_init(bar2, sizeof(bar2), 4096, 4096) == IVC_OK);
    assert(ivc_client_init(&zephyr, bar2, sizeof(bar2), IVC_PEER_ZEPHYR,
                           noop_doorbell, NULL, NULL, NULL) == IVC_OK);
    assert(ivc_client_init(&linux, bar2, sizeof(bar2), IVC_PEER_LINUX,
                           noop_doorbell, NULL, NULL, NULL) == IVC_OK);
    ivc_client_set_pending_table(&zephyr, zephyr_pending, PENDING_CAPACITY);

    for (uint32_t i = 0; i < sizeof(sizes) / sizeof(sizes[0]); i++) {
        uint8_t *image = (uint8_t *)malloc(sizes[i]);
        uint8_t *received = (uint8_t *)malloc(sizes[i]);
        struct ivc_message msg;
        const struct ivc_image_desc *desc;
        uint32_t payload_len = 0;
        uint64_t seq = 0;

        assert(image != NULL);
        assert(received != NULL);
        fill_pattern(image, sizes[i], i + 1U);

        assert(ivc_demo_make_image_desc(
                   &zephyr, desc_payload, sizeof(desc_payload), 1000U + i,
                   640, 480, IVC_PIXEL_FORMAT_GRAY8, image, sizes[i],
                   &payload_len) == IVC_OK);
        assert(payload_len == sizeof(struct ivc_image_desc));
        assert(ivc_client_send_request(&zephyr, IVC_MSG_IMAGE_FRAME,
                                       desc_payload, payload_len, 1000U + i,
                                       i, 1000U, &seq) == IVC_OK);

        prepare_message(&msg, rx_desc_payload, sizeof(rx_desc_payload));
        assert(ivc_client_recv(&linux, &msg, IVC_RECV_NO_WAIT) == IVC_OK);
        assert(msg.payload_len == sizeof(struct ivc_image_desc));
        assert(ivc_demo_parse_image_desc(&msg, &desc) == IVC_OK);
        assert(desc->image_id == 1000U + i);
        assert(desc->data_len == sizes[i]);
        assert(desc->data_offset >= linux.endpoint.shared->data_offset);
        assert(desc->data_offset + desc->data_len <=
               linux.endpoint.shared->data_offset +
                   linux.endpoint.shared->data_size);
        if (reusable_offset == 0) {
            reusable_offset = desc->data_offset;
        } else {
            assert(desc->data_offset == reusable_offset);
        }

        memset(received, 0, sizes[i]);
        assert(ivc_demo_read_image_desc(&linux, desc, received, sizes[i]) ==
               IVC_OK);
        assert(memcmp(image, received, sizes[i]) == 0);
        assert(ivc_demo_release_image_desc(&linux, desc) == IVC_OK);
        assert(ivc_data_read(&linux.endpoint, desc->data_offset, received,
                             sizes[i]) == IVC_ERR_NOT_FOUND);
        printf("ivc image desc size=%u ring_payload=%u offset=%llu pass\n",
               sizes[i], msg.payload_len,
               (unsigned long long)desc->data_offset);

        free(received);
        free(image);
    }

    puts("ivc image descriptor smoke pass");
    return 0;
}
