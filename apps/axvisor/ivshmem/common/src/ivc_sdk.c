/* SPDX-License-Identifier: Apache-2.0 */
#include "ivc_sdk.h"

#include <string.h>

int ivc_sdk_shared_init(void *bar2, uint32_t total_size,
                        uint32_t z_to_l_size, uint32_t l_to_z_size)
{
    return ivc_shared_init(bar2, total_size, z_to_l_size, l_to_z_size);
}

int ivc_sdk_init(struct ivc_sdk *sdk, void *bar2, uint32_t total_size,
                 enum ivc_peer peer, ivc_doorbell_fn doorbell,
                 void *doorbell_ctx, ivc_wait_fn wait, void *wait_ctx)
{
    if (sdk == NULL) {
        return IVC_ERR_INVALID_ARG;
    }
    memset(sdk, 0, sizeof(*sdk));
    return ivc_client_init(&sdk->client, bar2, total_size, peer, doorbell,
                           doorbell_ctx, wait, wait_ctx);
}

void ivc_sdk_set_pending_table(struct ivc_sdk *sdk,
                               struct ivc_pending_entry *entries,
                               uint32_t capacity)
{
    if (sdk == NULL) {
        return;
    }
    ivc_client_set_pending_table(&sdk->client, entries, capacity);
}

int ivc_sdk_recv(struct ivc_sdk *sdk, struct ivc_message *msg,
                 int timeout_ms)
{
    if (sdk == NULL) {
        return IVC_ERR_INVALID_ARG;
    }
    return ivc_client_recv(&sdk->client, msg, timeout_ms);
}

int ivc_sdk_complete_reply(struct ivc_sdk *sdk,
                           const struct ivc_message *reply,
                           struct ivc_pending_entry *entry_out)
{
    if (sdk == NULL) {
        return IVC_ERR_INVALID_ARG;
    }
    return ivc_client_complete_reply(&sdk->client, reply, entry_out);
}

int ivc_sdk_send_image(struct ivc_sdk *sdk,
                       const struct ivc_sdk_image *image, uint64_t now_ms,
                       uint32_t timeout_ms, uint64_t *seq_out)
{
    uint8_t payload[sizeof(struct ivc_image_desc)];
    uint32_t payload_len = 0;
    int rc;

    if (sdk == NULL || image == NULL ||
        (image->data == NULL && image->data_len != 0U)) {
        return IVC_ERR_INVALID_ARG;
    }

    rc = ivc_demo_make_image_desc(&sdk->client, payload, sizeof(payload),
                                  image->image_id, image->width,
                                  image->height, image->pixel_format,
                                  image->data, image->data_len,
                                  &payload_len);
    if (rc != IVC_OK) {
        return rc;
    }

    rc = ivc_client_send_request(&sdk->client, IVC_MSG_IMAGE_FRAME, payload,
                                 payload_len, image->image_id, now_ms,
                                 timeout_ms, seq_out);
    if (rc != IVC_OK) {
        const struct ivc_image_desc *desc =
            (const struct ivc_image_desc *)payload;
        (void)ivc_demo_release_image_desc(&sdk->client, desc);
    }
    return rc;
}

int ivc_sdk_recv_image(const struct ivc_message *msg,
                       struct ivc_sdk_received_image *image_out)
{
    const struct ivc_image_desc *desc;
    int rc;

    if (image_out == NULL) {
        return IVC_ERR_INVALID_ARG;
    }
    rc = ivc_demo_parse_image_desc(msg, &desc);
    if (rc != IVC_OK) {
        return rc;
    }

    image_out->image_id = desc->image_id;
    image_out->width = desc->width;
    image_out->height = desc->height;
    image_out->pixel_format = desc->pixel_format;
    image_out->data_len = desc->data_len;
    image_out->data_offset = desc->data_offset;
    return IVC_OK;
}

int ivc_sdk_read_image(const struct ivc_sdk *sdk,
                       const struct ivc_sdk_received_image *image,
                       void *data, uint32_t capacity)
{
    struct ivc_image_desc desc;

    if (sdk == NULL || image == NULL) {
        return IVC_ERR_INVALID_ARG;
    }

    desc.image_id = image->image_id;
    desc.width = image->width;
    desc.height = image->height;
    desc.pixel_format = image->pixel_format;
    desc.data_len = image->data_len;
    desc.data_offset = image->data_offset;
    return ivc_demo_read_image_desc(&sdk->client, &desc, data, capacity);
}

int ivc_sdk_release_image(struct ivc_sdk *sdk,
                          const struct ivc_sdk_received_image *image)
{
    struct ivc_image_desc desc;

    if (sdk == NULL || image == NULL) {
        return IVC_ERR_INVALID_ARG;
    }

    desc.image_id = image->image_id;
    desc.width = image->width;
    desc.height = image->height;
    desc.pixel_format = image->pixel_format;
    desc.data_len = image->data_len;
    desc.data_offset = image->data_offset;
    return ivc_demo_release_image_desc(&sdk->client, &desc);
}

int ivc_sdk_send_control(struct ivc_sdk *sdk,
                         const struct ivc_sdk_control *control,
                         uint64_t reply_to, uint64_t now_ms,
                         uint32_t timeout_ms, uint64_t *seq_out)
{
    uint8_t payload[sizeof(struct ivc_control_cmd) + IVC_SDK_CONTROL_ARGS_MAX];
    uint32_t payload_len = 0;
    int rc;

    if (sdk == NULL || control == NULL ||
        (control->args == NULL && control->arg_len != 0U)) {
        return IVC_ERR_INVALID_ARG;
    }
    if (control->arg_len > IVC_SDK_CONTROL_ARGS_MAX) {
        return IVC_ERR_PAYLOAD_TOO_LARGE;
    }

    rc = ivc_demo_make_control_cmd(
        payload, sizeof(payload), control->command, control->flags,
        control->target_id, control->args, control->arg_len, &payload_len);
    if (rc != IVC_OK) {
        return rc;
    }

    return ivc_client_send_request_to(&sdk->client, reply_to,
                                      IVC_MSG_CONTROL_CMD, payload,
                                      payload_len, control->target_id,
                                      now_ms, timeout_ms, seq_out);
}

int ivc_sdk_recv_control(const struct ivc_message *msg,
                         struct ivc_sdk_control_view *control_out)
{
    const struct ivc_control_cmd *cmd;
    int rc;

    if (control_out == NULL) {
        return IVC_ERR_INVALID_ARG;
    }
    rc = ivc_demo_parse_control_cmd(msg, &cmd);
    if (rc != IVC_OK) {
        return rc;
    }

    control_out->command = cmd->command;
    control_out->flags = cmd->flags;
    control_out->target_id = cmd->target_id;
    control_out->args = cmd->args;
    control_out->arg_len = cmd->arg_len;
    return IVC_OK;
}

int ivc_sdk_reply_control_result(struct ivc_sdk *sdk,
                                 const struct ivc_message *request,
                                 uint32_t command, int32_t status,
                                 uint64_t target_id, uint64_t *seq_out)
{
    struct ivc_control_result result;
    uint32_t msg_type;

    if (sdk == NULL || request == NULL) {
        return IVC_ERR_INVALID_ARG;
    }

    result.command = command;
    result.status = status;
    result.target_id = target_id;
    msg_type = status == IVC_CONTROL_OK ? IVC_MSG_CONTROL_DONE :
                                          IVC_MSG_CONTROL_FAILED;
    return ivc_client_reply(&sdk->client, request, msg_type, &result,
                            sizeof(result), seq_out);
}

int ivc_sdk_recv_control_result(
    const struct ivc_message *msg,
    struct ivc_sdk_control_result_view *result_out)
{
    const struct ivc_control_result *result;

    if (msg == NULL || result_out == NULL || msg->payload == NULL ||
        (msg->header.msg_type != IVC_MSG_CONTROL_DONE &&
         msg->header.msg_type != IVC_MSG_CONTROL_FAILED) ||
        msg->payload_len != sizeof(struct ivc_control_result)) {
        return IVC_ERR_INVALID_ARG;
    }

    result = (const struct ivc_control_result *)msg->payload;
    result_out->command = result->command;
    result_out->status = result->status;
    result_out->target_id = result->target_id;
    return IVC_OK;
}
