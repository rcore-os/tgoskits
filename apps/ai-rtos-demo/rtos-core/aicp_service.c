// Copyright 2026 The TGOSKits Authors
//
// SPDX-License-Identifier: Apache-2.0

#include "aicp_service.h"

#include <string.h>

static uint64_t monotonic_ns(const struct aicp_service_ops *ops) {
    if (ops == NULL || ops->monotonic_ns == NULL) {
        return 0;
    }
    return ops->monotonic_ns(ops->context);
}

static void emit_event(
    const struct aicp_service_ops *ops,
    enum aicp_service_event event,
    const struct aicp_header *header,
    const struct control_state *control,
    int result,
    uint16_t error_code) {
    if (ops == NULL || ops->on_event == NULL) {
        return;
    }

    const struct aicp_service_event_data data = {
        .event = event,
        .header = header,
        .control = control,
        .result = result,
        .error_code = error_code,
    };
    ops->on_event(ops->context, &data);
}

static int send_status(
    struct aicp_stream *stream,
    const struct control_state *control,
    uint32_t sequence,
    const struct aicp_service_ops *ops,
    const struct aicp_header *request) {
    struct aicp_status_payload payload;
    uint8_t payload_wire[AICP_STATUS_PAYLOAD_LEN];

    control_status(control, &payload);
    aicp_status_payload_encode(&payload, payload_wire);
    struct aicp_header response = aicp_make_header(
        AICP_MSG_STATUS,
        0,
        AICP_STATUS_PAYLOAD_LEN,
        sequence,
        monotonic_ns(ops),
        AICP_OK);
    int result = aicp_stream_send_frame(stream, response, payload_wire);
    if (result == 0) {
        emit_event(ops, AICP_SERVICE_STATUS_SENT, request, control, 0, AICP_OK);
    }
    return result;
}

static int send_error(
    struct aicp_stream *stream,
    uint32_t sequence,
    uint16_t error_code,
    const struct aicp_service_ops *ops,
    const struct aicp_header *request) {
    static const char payload[] = "{\"error\":\"invalid AICP frame\"}";
    struct aicp_header response = aicp_make_header(
        AICP_MSG_ERROR,
        0,
        sizeof(payload),
        sequence,
        monotonic_ns(ops),
        error_code);
    int result = aicp_stream_send_frame(stream, response, payload);
    if (result == 0) {
        emit_event(ops, AICP_SERVICE_ERROR_SENT, request, NULL, 0, error_code);
    }
    return result;
}

static bool sequence_is_newer(uint32_t current, uint32_t previous) {
    return (int32_t)(current - previous) > 0;
}

static int replay_last_reply(
    struct aicp_stream *stream,
    const struct aicp_service_session *session,
    const struct aicp_service_ops *ops,
    const struct aicp_header *request) {
    switch (session->sequence.reply) {
    case AICP_CACHED_REPLY_STATUS:
        return send_status(
            stream, &session->control, session->sequence.last_seq, ops, request);
    case AICP_CACHED_REPLY_ERROR:
        return send_error(
            stream,
            session->sequence.last_seq,
            session->sequence.error_code,
            ops,
            request);
    case AICP_CACHED_REPLY_NONE:
    default:
        return 0;
    }
}

void aicp_service_session_init(struct aicp_service_session *session) {
    memset(session, 0, sizeof(*session));
    control_state_init(&session->control);
}

void aicp_service_stats_init(struct aicp_service_stats *stats) {
    memset(stats, 0, sizeof(*stats));
}

int aicp_service_serve(
    struct aicp_stream *stream,
    struct aicp_service_session *session,
    struct aicp_service_stats *stats,
    const struct aicp_service_ops *ops) {
    for (;;) {
        struct aicp_header header;
        int result =
            aicp_stream_recv_frame(
                stream, &header, session->payload, sizeof(session->payload));
        if (result != 0) {
            emit_event(ops, AICP_SERVICE_DISCONNECTED, NULL, NULL, result, AICP_OK);
            return result;
        }

        stats->received_frames++;
        emit_event(
            ops, AICP_SERVICE_FRAME_RECEIVED, &header, &session->control, 0, AICP_OK);

        if (header.version != AICP_VERSION) {
            stats->protocol_errors++;
            result = send_error(
                stream, header.seq, AICP_ERR_VERSION, ops, &header);
            if (result != 0) {
                return result;
            }
            continue;
        }

        if (session->sequence.valid && header.seq == session->sequence.last_seq) {
            stats->duplicate_requests++;
            emit_event(
                ops, AICP_SERVICE_DUPLICATE, &header, &session->control, 0, AICP_OK);
            result = replay_last_reply(stream, session, ops, &header);
            if (result != 0) {
                return result;
            }
            continue;
        }

        if (session->sequence.valid &&
            !sequence_is_newer(header.seq, session->sequence.last_seq)) {
            stats->stale_requests++;
            stats->protocol_errors++;
            emit_event(
                ops,
                AICP_SERVICE_STALE,
                &header,
                &session->control,
                0,
                AICP_ERR_SEQUENCE);
            result = send_error(
                stream, header.seq, AICP_ERR_SEQUENCE, ops, &header);
            if (result != 0) {
                return result;
            }
            continue;
        }

        session->sequence.valid = true;
        session->sequence.last_seq = header.seq;
        session->sequence.reply = AICP_CACHED_REPLY_NONE;
        session->sequence.error_code = AICP_OK;

        switch (header.msg_type) {
        case AICP_MSG_HELLO:
            emit_event(ops, AICP_SERVICE_HELLO, &header, &session->control, 0, AICP_OK);
            session->sequence.reply = AICP_CACHED_REPLY_STATUS;
            result = send_status(stream, &session->control, header.seq, ops, &header);
            if (result != 0) {
                return result;
            }
            break;
        case AICP_MSG_HEARTBEAT:
            session->sequence.reply = AICP_CACHED_REPLY_STATUS;
            result = send_status(stream, &session->control, header.seq, ops, &header);
            if (result != 0) {
                return result;
            }
            break;
        case AICP_MSG_CONTROL_SET: {
            if (header.payload_len != AICP_CONTROL_PAYLOAD_LEN) {
                stats->protocol_errors++;
                session->sequence.reply = AICP_CACHED_REPLY_ERROR;
                session->sequence.error_code = AICP_ERR_BAD_PAYLOAD;
                result = send_error(
                    stream, header.seq, AICP_ERR_BAD_PAYLOAD, ops, &header);
                if (result != 0) {
                    return result;
                }
                break;
            }

            struct aicp_control_payload control;
            aicp_control_payload_decode(session->payload, &control);
            if (!aicp_control_payload_is_valid(&control)) {
                stats->protocol_errors++;
                session->sequence.reply = AICP_CACHED_REPLY_ERROR;
                session->sequence.error_code = AICP_ERR_BAD_PAYLOAD;
                result = send_error(
                    stream, header.seq, AICP_ERR_BAD_PAYLOAD, ops, &header);
                if (result != 0) {
                    return result;
                }
                break;
            }
            control_step(&session->control, &control, header.seq);
            stats->control_requests++;
            emit_event(
                ops,
                AICP_SERVICE_CONTROL_APPLIED,
                &header,
                &session->control,
                0,
                AICP_OK);
            session->sequence.reply = AICP_CACHED_REPLY_STATUS;
            result = send_status(stream, &session->control, header.seq, ops, &header);
            if (result != 0) {
                return result;
            }
            break;
        }
        default:
            stats->protocol_errors++;
            session->sequence.reply = AICP_CACHED_REPLY_ERROR;
            session->sequence.error_code = AICP_ERR_BAD_TYPE;
            result = send_error(stream, header.seq, AICP_ERR_BAD_TYPE, ops, &header);
            if (result != 0) {
                return result;
            }
            break;
        }
    }
}
