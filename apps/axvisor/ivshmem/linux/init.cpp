#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
#include "ivc_sdk.h"

#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/reboot.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#define IMAGE_BYTES (256U * 1024U)
#define TIMEOUT_LOOPS 60000U
#define POLL_INTERVAL_US 1000U

static uint8_t rx_image[IMAGE_BYTES];
static uint8_t msg_payload[512];

static void die_msg(const char *msg)
{
    fprintf(stderr, "ivshmem message protocol failed: %s: %s\n", msg,
            strerror(errno));
    sync();
    _exit(1);
}

static void fail_msg(const char *msg)
{
    fprintf(stderr, "ivshmem message protocol failed: %s\n", msg);
    sync();
    _exit(1);
}

static void signal_die(int sig)
{
    fprintf(stderr, "ivshmem message protocol failed: signal %d\n", sig);
    sync();
    _exit(128 + sig);
}

static void ensure_dir(const char *path)
{
    if (mkdir(path, 0755) != 0 && errno != EEXIST) {
        die_msg(path);
    }
}

static void mount_fs(const char *source, const char *target, const char *type)
{
    if (mount(source, target, type, 0, "") != 0 && errno != EBUSY) {
        die_msg(target);
    }
}

static void prepare_message(struct ivc_message *msg, void *payload,
                            uint32_t capacity)
{
    memset(msg, 0, sizeof(*msg));
    memset(payload, 0, capacity);
    msg->payload = payload;
    msg->payload_capacity = capacity;
}

static void recv_with_poll(struct ivc_sdk *sdk, struct ivc_message *msg,
                           const char *what)
{
    for (uint32_t i = 0; i < TIMEOUT_LOOPS; i++) {
        int rc = ivc_sdk_recv(sdk, msg, IVC_RECV_NO_WAIT);
        if (rc == IVC_OK) {
            return;
        }
        if (rc != IVC_ERR_EMPTY) {
            fprintf(stderr, "ivshmem message protocol failed: recv %s rc=%d\n",
                    what, rc);
            sync();
            _exit(1);
        }
        usleep(POLL_INTERVAL_US);
    }
    fprintf(stderr, "ivshmem message protocol failed: timeout waiting for %s\n",
            what);
    sync();
    _exit(1);
}

static void poweroff(void)
{
    sync();
    syscall(SYS_reboot, 0xfee1dead, 672274793, 0x4321fedc, NULL);
    for (;;) {
        pause();
    }
}

int main(void)
{
    struct ivc_sdk sdk {};
    struct ivc_message msg {};
    struct ivc_sdk_received_image image {};
    struct ivc_sdk_control control {};
    struct ivc_sdk_control_result_view result {};
    struct ivc_pending_entry completed {};
    uint64_t control_seq = 0;

    setbuf(stdout, NULL);
    setbuf(stderr, NULL);
    signal(SIGBUS, signal_die);
    signal(SIGSEGV, signal_die);

    ensure_dir("/proc");
    ensure_dir("/sys");
    ensure_dir("/dev");
    mount_fs("proc", "/proc", "proc");
    mount_fs("sysfs", "/sys", "sysfs");
    mount_fs("devtmpfs", "/dev", "devtmpfs");

    if (ivc_sdk_open_default(&sdk, IVC_PEER_LINUX) != IVC_OK) {
        die_msg("open default IVC SDK");
    }
    puts("Linux SDK ready");

    prepare_message(&msg, msg_payload, sizeof(msg_payload));
    recv_with_poll(&sdk, &msg, "Zephyr image");
    if (ivc_sdk_recv_image(&msg, &image) != IVC_OK) {
        fail_msg("parse Zephyr image failed");
    }
    if (image.data_len > sizeof(rx_image)) {
        fail_msg("Zephyr image too large");
    }
    if (ivc_sdk_read_image(&sdk, &image, rx_image, sizeof(rx_image)) !=
        IVC_OK) {
        fail_msg("read Zephyr image failed");
    }
    if (ivc_sdk_release_image(&sdk, &image) != IVC_OK) {
        fail_msg("release Zephyr image failed");
    }
    printf("Linux SDK receives image seq=%llu image_id=%llu bytes=%u\n",
           (unsigned long long)msg.header.seq,
           (unsigned long long)image.image_id, image.data_len);

    control.command = IVC_CMD_SET_EXPOSURE;
    control.flags = 0;
    control.target_id = image.image_id;
    control.args = "apply";
    control.arg_len = 6;
    if (ivc_sdk_send_control(&sdk, &control, msg.header.seq, 2000, 1000,
                             &control_seq) != IVC_OK) {
        fail_msg("send Linux control failed");
    }
    printf("Linux SDK sends control seq=%llu reply_to=%llu\n",
           (unsigned long long)control_seq,
           (unsigned long long)msg.header.seq);

    prepare_message(&msg, msg_payload, sizeof(msg_payload));
    recv_with_poll(&sdk, &msg, "Zephyr control result");
    if (ivc_sdk_complete_reply(&sdk, &msg, &completed) != IVC_OK) {
        fail_msg("complete Linux pending control failed");
    }
    if (completed.user_data != image.image_id) {
        fail_msg("Linux pending user data mismatch");
    }
    if (ivc_sdk_recv_control_result(&msg, &result) != IVC_OK) {
        fail_msg("parse Zephyr control result failed");
    }
    if (result.status != IVC_CONTROL_OK || result.target_id != image.image_id) {
        fail_msg("Zephyr control result mismatch");
    }
    printf("Linux SDK receives result reply_to=%llu status=%d\n",
           (unsigned long long)msg.header.reply_to, result.status);

    usleep(300000);
    puts("ivshmem message protocol pass");
    ivc_sdk_close(&sdk);
    poweroff();
    return 0;
}
