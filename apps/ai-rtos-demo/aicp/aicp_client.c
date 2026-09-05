// Copyright 2026 The TGOSKits Authors
//
// SPDX-License-Identifier: Apache-2.0

#include "aicp_client.h"

#include <errno.h>
#include <string.h>

static void emit_event(
    const struct aicp_client_ops *ops,
    enum aicp_client_event_kind kind,
    const struct aicp_header *request,
    const struct aicp_header *response,
    int result) {
    if (ops != NULL && ops->on_event != NULL) {
        const struct aicp_client_event event = {
            .kind = kind,
            .request = request,
            .response = response,
            .result = result,
        };
        ops->on_event(ops->context, &event);
    }
}

static uint64_t monotonic_ns(const struct aicp_client_ops *ops) {
    if (ops == NULL || ops->monotonic_ns == NULL) {
        return 0;
    }
    return ops->monotonic_ns(ops->context);
}

static int send_request(
    struct aicp_stream *stream,
    const struct aicp_header *request,
    const void *payload,
    const struct aicp_client_ops *ops) {
    emit_event(ops, AICP_CLIENT_TX_BEGIN, request, NULL, 0);
    const int result = aicp_stream_send_frame(stream, *request, payload);
    emit_event(ops, AICP_CLIENT_TX_COMPLETE, request, NULL, result);
    return result;
}

static int receive_status_response(
    struct aicp_stream *stream,
    const struct aicp_header *request,
    struct aicp_status_payload *status,
    uint64_t *rtt_ns,
    uint64_t start,
    const struct aicp_client_ops *ops) {
    uint8_t payload[AICP_MAX_PAYLOAD];
    struct aicp_header response;
    int result = aicp_stream_recv_frame(stream, &response, payload, sizeof(payload));
    emit_event(
        ops,
        AICP_CLIENT_RX_COMPLETE,
        request,
        result == 0 ? &response : NULL,
        result);
    if (result != 0) {
        return result;
    }

    if (rtt_ns != NULL) {
        *rtt_ns = monotonic_ns(ops) - start;
    }
    if (response.version != AICP_VERSION || response.msg_type == AICP_MSG_ERROR ||
        response.msg_type != AICP_MSG_STATUS || response.seq != request->seq ||
        response.payload_len != AICP_STATUS_PAYLOAD_LEN) {
        return -EPROTO;
    }

    aicp_status_payload_decode(payload, status);
    return 0;
}

int aicp_client_session_handshake(
    struct aicp_stream *stream,
    uint32_t *next_seq,
    const void *payload,
    uint32_t payload_len,
    struct aicp_status_payload *status,
    const struct aicp_client_ops *ops) {
    if (next_seq == NULL || status == NULL || (payload_len != 0 && payload == NULL) ||
        payload_len > AICP_MAX_PAYLOAD) {
        return -EINVAL;
    }

    const struct aicp_header request = aicp_make_header(
        AICP_MSG_HELLO,
        0,
        payload_len,
        (*next_seq)++,
        monotonic_ns(ops),
        AICP_OK);
    int result = send_request(stream, &request, payload, ops);
    if (result != 0) {
        return result;
    }
    return receive_status_response(stream, &request, status, NULL, 0, ops);
}

int aicp_client_session_transact_control(
    struct aicp_stream *stream,
    uint32_t *next_seq,
    const struct aicp_control_payload *control,
    struct aicp_status_payload *status,
    uint64_t *rtt_ns,
    const struct aicp_client_ops *ops) {
    if (next_seq == NULL || control == NULL || status == NULL) {
        return -EINVAL;
    }

    uint8_t control_wire[AICP_CONTROL_PAYLOAD_LEN];
    aicp_control_payload_encode(control, control_wire);
    const uint64_t start = monotonic_ns(ops);
    const struct aicp_header request = aicp_make_header(
        AICP_MSG_CONTROL_SET,
        0,
        AICP_CONTROL_PAYLOAD_LEN,
        (*next_seq)++,
        start,
        AICP_OK);
    int result = send_request(stream, &request, control_wire, ops);
    if (result != 0) {
        return result;
    }

    return receive_status_response(stream, &request, status, rtt_ns, start, ops);
}
