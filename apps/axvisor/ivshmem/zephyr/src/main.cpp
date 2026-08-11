#include "ivc_sdk.h"

#include <stdint.h>
#include <string.h>
#include <zephyr/kernel.h>
#include <zephyr/sys/printk.h>

#define IMAGE_BYTES (256U * 1024U)
#define TIMEOUT_LOOPS 60000U
#define POLL_INTERVAL_US 1000U
#define PSCI_SYSTEM_OFF 0x84000008UL

static uint8_t tx_image[IMAGE_BYTES];
static uint8_t msg_payload[512];

static void psci_system_off(void)
{
    register uint64_t x0 __asm__("x0") = PSCI_SYSTEM_OFF;
    __asm__ volatile("smc #0" : "+r"(x0) : : "memory");
}

static void prepare_message(struct ivc_message *msg, void *payload,
                            uint32_t capacity)
{
    memset(msg, 0, sizeof(*msg));
    memset(payload, 0, capacity);
    msg->payload = payload;
    msg->payload_capacity = capacity;
}

static int recv_with_poll(struct ivc_sdk *sdk, struct ivc_message *msg,
                          const char *what)
{
    for (uint32_t i = 0; i < TIMEOUT_LOOPS; i++) {
        int rc = ivc_sdk_recv(sdk, msg, IVC_RECV_NO_WAIT);
        if (rc == IVC_OK) {
            return 0;
        }
        if (rc != IVC_ERR_EMPTY) {
            printk("Zephyr ivshmem failed: recv %s rc=%d\n", what, rc);
            return rc;
        }
        k_busy_wait(POLL_INTERVAL_US);
    }
    printk("Zephyr ivshmem failed: timeout waiting for %s\n", what);
    return IVC_ERR_TIMEOUT;
}

static void fill_image(uint8_t *data, uint32_t len)
{
    for (uint32_t i = 0; i < len; i++) {
        data[i] = (uint8_t)(i * 7U + 3U);
    }
}

int main(void)
{
    struct ivc_sdk sdk {};
    struct ivc_sdk_image image {};
    struct ivc_sdk_control_view control {};
    struct ivc_pending_entry completed {};
    struct ivc_message msg {};
    uint64_t image_seq = 0;

    printk("Zephyr ivshmem SDK peer init\n");
    if (ivc_sdk_open_default(&sdk, IVC_PEER_ZEPHYR) != IVC_OK) {
        return 1;
    }
    printk("Zephyr SDK ready\n");

    fill_image(tx_image, sizeof(tx_image));
    image.image_id = 42;
    image.width = 1024;
    image.height = 256;
    image.pixel_format = IVC_PIXEL_FORMAT_GRAY8;
    image.data = tx_image;
    image.data_len = sizeof(tx_image);
    if (ivc_sdk_send_image(&sdk, &image, 1000, 1000, &image_seq) != IVC_OK) {
        printk("Zephyr ivshmem failed: send image failed\n");
        return 1;
    }
    printk("Zephyr SDK sends image seq=%llu image_id=%llu bytes=%u\n",
           (unsigned long long)image_seq, (unsigned long long)image.image_id,
           image.data_len);

    prepare_message(&msg, msg_payload, sizeof(msg_payload));
    if (recv_with_poll(&sdk, &msg, "Linux control") != 0) {
        return 1;
    }
    if (ivc_sdk_recv_control(&msg, &control) != IVC_OK) {
        printk("Zephyr ivshmem failed: parse control failed\n");
        return 1;
    }
    if (ivc_sdk_complete_reply(&sdk, &msg, &completed) != IVC_OK ||
        completed.user_data != image.image_id) {
        printk("Zephyr ivshmem failed: complete image pending failed\n");
        return 1;
    }
    printk("Zephyr SDK receives control seq=%llu reply_to=%llu\n",
           (unsigned long long)msg.header.seq,
           (unsigned long long)msg.header.reply_to);
    printk("Zephyr executes CONTROL_CMD immediately\n");

    if (ivc_sdk_reply_control_result(&sdk, &msg, control.command,
                                     IVC_CONTROL_OK, control.target_id, NULL) !=
        IVC_OK) {
        printk("Zephyr ivshmem failed: reply control result failed\n");
        return 1;
    }
    printk("Zephyr SDK replies control result\n");

    k_sleep(K_MSEC(1000));
    psci_system_off();
    printk("Zephyr ivshmem failed: PSCI SYSTEM_OFF returned\n");
    return 1;
}
