// Copyright 2026 The TGOSKits Authors
//
// SPDX-License-Identifier: Apache-2.0

#ifndef TGOSKITS_AI_RTOS_DEMO_AICP_CLIENT_H
#define TGOSKITS_AI_RTOS_DEMO_AICP_CLIENT_H

#include "aicp_stream.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef uint64_t (*aicp_client_clock_fn)(void *context);

enum aicp_client_event_kind {
    AICP_CLIENT_TX_BEGIN,
    AICP_CLIENT_TX_COMPLETE,
    AICP_CLIENT_RX_COMPLETE,
};

struct aicp_client_event {
    enum aicp_client_event_kind kind;
    const struct aicp_header *request;
    const struct aicp_header *response;
    int result;
};

typedef void (*aicp_client_event_fn)(
    void *context,
    const struct aicp_client_event *event);

struct aicp_client_ops {
    aicp_client_clock_fn monotonic_ns;
    aicp_client_event_fn on_event;
    void *context;
};

/// Sends HELLO and consumes the STATUS reply that establishes a TCP session.
int aicp_client_session_handshake(
    struct aicp_stream *stream,
    uint32_t *next_seq,
    const void *payload,
    uint32_t payload_len,
    struct aicp_status_payload *status,
    const struct aicp_client_ops *ops);

int aicp_client_session_transact_control(
    struct aicp_stream *stream,
    uint32_t *next_seq,
    const struct aicp_control_payload *control,
    struct aicp_status_payload *status,
    uint64_t *rtt_ns,
    const struct aicp_client_ops *ops);

#ifdef __cplusplus
}
#endif

#endif
