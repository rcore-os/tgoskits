/* SPDX-License-Identifier: Apache-2.0 */
#ifndef AXVISOR_IVSHMEM_IVC_SDK_H
#define AXVISOR_IVSHMEM_IVC_SDK_H

#include "ivc_demo.h"

#ifdef __cplusplus
extern "C" {
#endif

#define IVC_SDK_CONTROL_ARGS_MAX 256U

struct ivc_sdk {
    struct ivc_client client;
};

struct ivc_sdk_image {
    uint64_t image_id;
    uint32_t width;
    uint32_t height;
    uint32_t pixel_format;
    const void *data;
    uint32_t data_len;
};

struct ivc_sdk_received_image {
    uint64_t image_id;
    uint32_t width;
    uint32_t height;
    uint32_t pixel_format;
    uint32_t data_len;
    uint64_t data_offset;
};

struct ivc_sdk_control {
    uint32_t command;
    uint32_t flags;
    uint64_t target_id;
    const void *args;
    uint32_t arg_len;
};

struct ivc_sdk_control_view {
    uint32_t command;
    uint32_t flags;
    uint64_t target_id;
    const void *args;
    uint32_t arg_len;
};

struct ivc_sdk_control_result_view {
    uint32_t command;
    int32_t status;
    uint64_t target_id;
};

int ivc_sdk_shared_init(void *bar2, uint32_t total_size,
                        uint32_t z_to_l_size, uint32_t l_to_z_size);
int ivc_sdk_init(struct ivc_sdk *sdk, void *bar2, uint32_t total_size,
                 enum ivc_peer peer, ivc_doorbell_fn doorbell,
                 void *doorbell_ctx, ivc_wait_fn wait, void *wait_ctx);
int ivc_sdk_open_default(struct ivc_sdk *sdk, enum ivc_peer peer);
void ivc_sdk_close(struct ivc_sdk *sdk);
void ivc_sdk_set_pending_table(struct ivc_sdk *sdk,
                               struct ivc_pending_entry *entries,
                               uint32_t capacity);

int ivc_sdk_recv(struct ivc_sdk *sdk, struct ivc_message *msg,
                 int timeout_ms);
int ivc_sdk_complete_reply(struct ivc_sdk *sdk,
                           const struct ivc_message *reply,
                           struct ivc_pending_entry *entry_out);

int ivc_sdk_send_image(struct ivc_sdk *sdk,
                       const struct ivc_sdk_image *image, uint64_t now_ms,
                       uint32_t timeout_ms, uint64_t *seq_out);
int ivc_sdk_recv_image(const struct ivc_message *msg,
                       struct ivc_sdk_received_image *image_out);
int ivc_sdk_read_image(const struct ivc_sdk *sdk,
                       const struct ivc_sdk_received_image *image,
                       void *data, uint32_t capacity);
int ivc_sdk_release_image(struct ivc_sdk *sdk,
                          const struct ivc_sdk_received_image *image);

int ivc_sdk_send_control(struct ivc_sdk *sdk,
                         const struct ivc_sdk_control *control,
                         uint64_t reply_to, uint64_t now_ms,
                         uint32_t timeout_ms, uint64_t *seq_out);
int ivc_sdk_recv_control(const struct ivc_message *msg,
                         struct ivc_sdk_control_view *control_out);
int ivc_sdk_reply_control_result(struct ivc_sdk *sdk,
                                 const struct ivc_message *request,
                                 uint32_t command, int32_t status,
                                 uint64_t target_id, uint64_t *seq_out);
int ivc_sdk_recv_control_result(
    const struct ivc_message *msg,
    struct ivc_sdk_control_result_view *result_out);

#ifdef __cplusplus
}
#endif

#endif /* AXVISOR_IVSHMEM_IVC_SDK_H */
