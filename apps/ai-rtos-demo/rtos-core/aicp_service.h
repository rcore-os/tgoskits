// Copyright 2026 The TGOSKits Authors
//
// SPDX-License-Identifier: Apache-2.0

#ifndef TGOSKITS_AI_RTOS_DEMO_AICP_SERVICE_H
#define TGOSKITS_AI_RTOS_DEMO_AICP_SERVICE_H

#include "aicp_stream.h"
#include "control_loop.h"

#include <stdbool.h>
#include <stdint.h>

enum aicp_service_event {
    AICP_SERVICE_FRAME_RECEIVED,
    AICP_SERVICE_HELLO,
    AICP_SERVICE_CONTROL_APPLIED,
    AICP_SERVICE_STATUS_SENT,
    AICP_SERVICE_ERROR_SENT,
    AICP_SERVICE_DUPLICATE,
    AICP_SERVICE_STALE,
    AICP_SERVICE_DISCONNECTED,
};

enum aicp_cached_reply {
    AICP_CACHED_REPLY_NONE,
    AICP_CACHED_REPLY_STATUS,
    AICP_CACHED_REPLY_ERROR,
};

struct aicp_sequence_state {
    bool valid;
    uint32_t last_seq;
    enum aicp_cached_reply reply;
    uint16_t error_code;
};

struct aicp_service_session {
    uint8_t payload[AICP_MAX_PAYLOAD];
    struct control_state control;
    struct aicp_sequence_state sequence;
};

struct aicp_service_stats {
    uint32_t received_frames;
    uint32_t control_requests;
    uint32_t protocol_errors;
    uint32_t duplicate_requests;
    uint32_t stale_requests;
};

struct aicp_service_event_data {
    enum aicp_service_event event;
    const struct aicp_header *header;
    const struct control_state *control;
    int result;
    uint16_t error_code;
};

struct aicp_service_ops {
    uint64_t (*monotonic_ns)(void *context);
    void (*on_event)(void *context, const struct aicp_service_event_data *event);
    void *context;
};

void aicp_service_session_init(struct aicp_service_session *session);
void aicp_service_stats_init(struct aicp_service_stats *stats);

int aicp_service_serve(
    struct aicp_stream *stream,
    struct aicp_service_session *session,
    struct aicp_service_stats *stats,
    const struct aicp_service_ops *ops);

#endif
