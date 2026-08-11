/* SPDX-License-Identifier: Apache-2.0 */
#include "ivc_demo.h"

#include <string.h>

int ivc_demo_make_image_frame(void *buffer, uint32_t capacity,
                              uint64_t image_id, uint32_t width,
                              uint32_t height, uint32_t pixel_format,
                              const void *image_data, uint32_t image_len,
                              uint32_t *payload_len_out)
{
    struct ivc_image_frame *frame = (struct ivc_image_frame *)buffer;
    uint32_t payload_len = (uint32_t)sizeof(*frame) + image_len;

    if (buffer == NULL || payload_len_out == NULL ||
        (image_data == NULL && image_len != 0U)) {
        return IVC_ERR_INVALID_ARG;
    }
    if (capacity < payload_len) {
        return IVC_ERR_PAYLOAD_TOO_LARGE;
    }

    frame->image_id = image_id;
    frame->width = width;
    frame->height = height;
    frame->pixel_format = pixel_format;
    frame->data_len = image_len;
    if (image_len != 0U) {
        memcpy(frame->data, image_data, image_len);
    }
    *payload_len_out = payload_len;
    return IVC_OK;
}

int ivc_demo_parse_image_frame(const struct ivc_message *msg,
                               const struct ivc_image_frame **image_out)
{
    const struct ivc_image_frame *frame;

    if (msg == NULL || image_out == NULL ||
        msg->header.msg_type != IVC_MSG_IMAGE_FRAME ||
        msg->payload == NULL ||
        msg->payload_len < sizeof(struct ivc_image_frame)) {
        return IVC_ERR_INVALID_ARG;
    }

    frame = (const struct ivc_image_frame *)msg->payload;
    if (msg->payload_len != sizeof(*frame) + frame->data_len) {
        return IVC_ERR_CORRUPT;
    }
    *image_out = frame;
    return IVC_OK;
}

int ivc_demo_make_image_desc(struct ivc_client *client, void *buffer,
                             uint32_t capacity, uint64_t image_id,
                             uint32_t width, uint32_t height,
                             uint32_t pixel_format, const void *image_data,
                             uint32_t image_len,
                             uint32_t *payload_len_out)
{
    struct ivc_image_desc *desc = (struct ivc_image_desc *)buffer;
    uint64_t data_offset = 0;
    int rc;

    if (client == NULL || buffer == NULL || payload_len_out == NULL ||
        (image_data == NULL && image_len != 0U)) {
        return IVC_ERR_INVALID_ARG;
    }
    if (capacity < sizeof(*desc)) {
        return IVC_ERR_PAYLOAD_TOO_LARGE;
    }

    rc = ivc_data_alloc(&client->endpoint, image_len, &data_offset);
    if (rc != IVC_OK) {
        return rc;
    }
    rc = ivc_data_write(&client->endpoint, data_offset, image_data, image_len);
    if (rc != IVC_OK) {
        (void)ivc_data_release(&client->endpoint, data_offset);
        return rc;
    }

    desc->image_id = image_id;
    desc->width = width;
    desc->height = height;
    desc->pixel_format = pixel_format;
    desc->data_len = image_len;
    desc->data_offset = data_offset;
    *payload_len_out = (uint32_t)sizeof(*desc);
    return IVC_OK;
}

int ivc_demo_parse_image_desc(const struct ivc_message *msg,
                              const struct ivc_image_desc **desc_out)
{
    if (msg == NULL || desc_out == NULL ||
        msg->header.msg_type != IVC_MSG_IMAGE_FRAME ||
        msg->payload == NULL ||
        msg->payload_len != sizeof(struct ivc_image_desc)) {
        return IVC_ERR_INVALID_ARG;
    }

    *desc_out = (const struct ivc_image_desc *)msg->payload;
    return IVC_OK;
}

int ivc_demo_read_image_desc(const struct ivc_client *client,
                             const struct ivc_image_desc *desc, void *data,
                             uint32_t capacity)
{
    if (client == NULL || desc == NULL || data == NULL) {
        return IVC_ERR_INVALID_ARG;
    }
    if (capacity < desc->data_len) {
        return IVC_ERR_PAYLOAD_TOO_LARGE;
    }
    return ivc_data_read(&client->endpoint, desc->data_offset, data,
                         desc->data_len);
}

int ivc_demo_release_image_desc(struct ivc_client *client,
                                const struct ivc_image_desc *desc)
{
    if (client == NULL || desc == NULL) {
        return IVC_ERR_INVALID_ARG;
    }
    return ivc_data_release(&client->endpoint, desc->data_offset);
}

int ivc_demo_make_control_cmd(void *buffer, uint32_t capacity,
                              uint32_t command, uint32_t flags,
                              uint64_t target_id, const void *args,
                              uint32_t arg_len,
                              uint32_t *payload_len_out)
{
    struct ivc_control_cmd *cmd = (struct ivc_control_cmd *)buffer;
    uint32_t payload_len = (uint32_t)sizeof(*cmd) + arg_len;

    if (buffer == NULL || payload_len_out == NULL ||
        (args == NULL && arg_len != 0U)) {
        return IVC_ERR_INVALID_ARG;
    }
    if (capacity < payload_len) {
        return IVC_ERR_PAYLOAD_TOO_LARGE;
    }

    cmd->command = command;
    cmd->flags = flags;
    cmd->target_id = target_id;
    cmd->arg_len = arg_len;
    if (arg_len != 0U) {
        memcpy(cmd->args, args, arg_len);
    }
    *payload_len_out = payload_len;
    return IVC_OK;
}

int ivc_demo_parse_control_cmd(const struct ivc_message *msg,
                               const struct ivc_control_cmd **cmd_out)
{
    const struct ivc_control_cmd *cmd;

    if (msg == NULL || cmd_out == NULL ||
        msg->header.msg_type != IVC_MSG_CONTROL_CMD ||
        msg->payload == NULL ||
        msg->payload_len < sizeof(struct ivc_control_cmd)) {
        return IVC_ERR_INVALID_ARG;
    }

    cmd = (const struct ivc_control_cmd *)msg->payload;
    if (msg->payload_len != sizeof(*cmd) + cmd->arg_len) {
        return IVC_ERR_CORRUPT;
    }
    *cmd_out = cmd;
    return IVC_OK;
}

int ivc_demo_execute_control_cmd(const struct ivc_control_cmd *cmd,
                                 struct ivc_control_result *result,
                                 uint32_t *reply_type_out)
{
    int32_t status = IVC_CONTROL_OK;

    if (cmd == NULL || result == NULL || reply_type_out == NULL) {
        return IVC_ERR_INVALID_ARG;
    }

    switch (cmd->command) {
    case IVC_CMD_SET_EXPOSURE:
    case IVC_CMD_SET_GAIN:
    case IVC_CMD_CAPTURE_ONCE:
    case IVC_CMD_STOP_CAPTURE:
        status = IVC_CONTROL_OK;
        break;
    default:
        status = IVC_CONTROL_ERR_UNKNOWN_COMMAND;
        break;
    }

    result->command = cmd->command;
    result->status = status;
    result->target_id = cmd->target_id;
    *reply_type_out = status == IVC_CONTROL_OK ? IVC_MSG_CONTROL_DONE :
                                               IVC_MSG_CONTROL_FAILED;
    return IVC_OK;
}

const char *ivc_demo_control_status_string(int32_t status)
{
    switch (status) {
    case IVC_CONTROL_OK:
        return "ok";
    case IVC_CONTROL_ERR_UNKNOWN_COMMAND:
        return "unknown command";
    case IVC_CONTROL_ERR_INVALID_ARGUMENT:
        return "invalid argument";
    default:
        return "unknown status";
    }
}

static uint32_t ivc_demo_strlen(const char *s)
{
    uint32_t len = 0;

    if (s == NULL) {
        return 0;
    }
    while (s[len] != '\0') {
        len++;
    }
    return len;
}

int ivc_demo_make_error(void *buffer, uint32_t capacity, uint32_t code,
                        const char *detail, uint32_t *payload_len_out)
{
    struct ivc_error_payload *error = (struct ivc_error_payload *)buffer;
    uint32_t detail_len = ivc_demo_strlen(detail);
    uint32_t payload_len = (uint32_t)sizeof(*error) + detail_len;

    if (buffer == NULL || payload_len_out == NULL) {
        return IVC_ERR_INVALID_ARG;
    }
    if (capacity < payload_len) {
        return IVC_ERR_PAYLOAD_TOO_LARGE;
    }

    error->code = code;
    error->detail_len = detail_len;
    if (detail_len != 0U) {
        memcpy(error->detail, detail, detail_len);
    }
    *payload_len_out = payload_len;
    return IVC_OK;
}

int ivc_demo_parse_error(const struct ivc_message *msg,
                         const struct ivc_error_payload **error_out)
{
    const struct ivc_error_payload *error;

    if (msg == NULL || error_out == NULL ||
        (msg->header.msg_type != IVC_MSG_ERROR &&
         msg->header.msg_type != IVC_MSG_CONTROL_FAILED) ||
        msg->payload == NULL ||
        msg->payload_len < sizeof(struct ivc_error_payload)) {
        return IVC_ERR_INVALID_ARG;
    }

    error = (const struct ivc_error_payload *)msg->payload;
    if (msg->payload_len != sizeof(*error) + error->detail_len) {
        return IVC_ERR_CORRUPT;
    }
    *error_out = error;
    return IVC_OK;
}

const char *ivc_demo_error_code_string(uint32_t code)
{
    switch (code) {
    case IVC_PROTO_ERR_INVALID_MAGIC:
        return "invalid magic";
    case IVC_PROTO_ERR_UNSUPPORTED_VERSION:
        return "unsupported version";
    case IVC_PROTO_ERR_RING_FULL:
        return "ring full";
    case IVC_PROTO_ERR_CHECKSUM:
        return "checksum error";
    case IVC_PROTO_ERR_TIMEOUT:
        return "timeout";
    case IVC_PROTO_ERR_UNKNOWN_COMMAND:
        return "unknown command";
    default:
        return "unknown error";
    }
}

int ivc_demo_make_heartbeat(void *buffer, uint32_t capacity, uint64_t nonce,
                            uint64_t uptime_ms, uint32_t *payload_len_out)
{
    struct ivc_heartbeat_payload *heartbeat =
        (struct ivc_heartbeat_payload *)buffer;

    if (buffer == NULL || payload_len_out == NULL) {
        return IVC_ERR_INVALID_ARG;
    }
    if (capacity < sizeof(*heartbeat)) {
        return IVC_ERR_PAYLOAD_TOO_LARGE;
    }

    heartbeat->nonce = nonce;
    heartbeat->uptime_ms = uptime_ms;
    *payload_len_out = (uint32_t)sizeof(*heartbeat);
    return IVC_OK;
}

int ivc_demo_parse_heartbeat(
    const struct ivc_message *msg,
    const struct ivc_heartbeat_payload **heartbeat_out)
{
    if (msg == NULL || heartbeat_out == NULL ||
        msg->header.msg_type != IVC_MSG_HEARTBEAT || msg->payload == NULL ||
        msg->payload_len != sizeof(struct ivc_heartbeat_payload)) {
        return IVC_ERR_INVALID_ARG;
    }

    *heartbeat_out = (const struct ivc_heartbeat_payload *)msg->payload;
    return IVC_OK;
}
