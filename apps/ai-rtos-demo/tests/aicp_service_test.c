// Copyright 2026 The TGOSKits Authors
//
// SPDX-License-Identifier: Apache-2.0

#include "aicp_service.h"
#include "aicp_posix_stream.h"

#include <errno.h>
#include <math.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

#define EVENT_COUNT (AICP_SERVICE_DISCONNECTED + 1)

struct server_context {
    int socket;
    int result;
    uint64_t clock_ns;
    unsigned events[EVENT_COUNT];
    struct aicp_service_session session;
    struct aicp_service_stats stats;
    struct aicp_service_ops ops;
};

static uint64_t test_monotonic_ns(void *opaque) {
    struct server_context *context = opaque;
    context->clock_ns += 1000;
    return context->clock_ns;
}

static void record_event(void *opaque, const struct aicp_service_event_data *event) {
    struct server_context *context = opaque;
    if ((unsigned)event->event < EVENT_COUNT) {
        context->events[event->event]++;
    }
}

static void *serve(void *opaque) {
    struct server_context *context = opaque;
    struct aicp_posix_stream stream;

    aicp_posix_stream_init(&stream, context->socket);
    context->result = aicp_service_serve(
        &stream.stream, &context->session, &context->stats, &context->ops);
    return NULL;
}

static int write_raw_frame(int socket, struct aicp_header header, const void *payload) {
    uint8_t wire[AICP_HEADER_LEN];
    header.crc16 = aicp_frame_crc(header, payload);
    aicp_header_encode(&header, wire);

    int result = aicp_posix_write_full(socket, wire, sizeof(wire));
    if (result != 0 || header.payload_len == 0) {
        return result;
    }
    return aicp_posix_write_full(socket, payload, header.payload_len);
}

static int expect_reply(
    int socket,
    uint8_t message_type,
    uint32_t sequence,
    uint16_t error_code,
    struct aicp_status_payload *status) {
    uint8_t payload[AICP_MAX_PAYLOAD];
    struct aicp_header header;
    int result = aicp_posix_recv_frame(
        socket, &header, payload, sizeof(payload));
    if (result != 0) {
        fprintf(stderr, "reply receive failed: %d\n", result);
        return 1;
    }
    if (header.msg_type != message_type || header.seq != sequence ||
        header.error_code != error_code) {
        fprintf(
            stderr,
            "unexpected reply: type=%u seq=%u error=%u\n",
            header.msg_type,
            header.seq,
            header.error_code);
        return 1;
    }
    if (status != NULL) {
        if (header.payload_len != AICP_STATUS_PAYLOAD_LEN) {
            fprintf(stderr, "unexpected status payload length: %u\n", header.payload_len);
            return 1;
        }
        aicp_status_payload_decode(payload, status);
    }
    return 0;
}

static int send_control_payload(
    int socket,
    uint32_t sequence,
    const struct aicp_control_payload *payload) {
    uint8_t payload_wire[AICP_CONTROL_PAYLOAD_LEN];
    aicp_control_payload_encode(payload, payload_wire);
    struct aicp_header header = aicp_make_header(
        AICP_MSG_CONTROL_SET,
        0,
        AICP_CONTROL_PAYLOAD_LEN,
        sequence,
        sequence * 1000,
        AICP_OK);
    return aicp_posix_send_frame(socket, header, payload_wire);
}

static int send_control(int socket, uint32_t sequence, float target) {
    const struct aicp_control_payload payload = {
        .target = target,
        .kp = 0.65f,
        .ki = 0.08f,
        .kd = 0.03f,
        .feed_forward = 0.05f,
        .mode = 1,
    };
    return send_control_payload(socket, sequence, &payload);
}

static int run_service_sequence_test(void) {
    int sockets[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sockets) != 0) {
        perror("socketpair");
        return 1;
    }

    struct server_context context = {
        .socket = sockets[1],
        .result = 0,
        .clock_ns = 1000000,
    };
    aicp_service_session_init(&context.session);
    aicp_service_stats_init(&context.stats);
    context.ops.monotonic_ns = test_monotonic_ns;
    context.ops.on_event = record_event;
    context.ops.context = &context;

    pthread_t thread;
    if (pthread_create(&thread, NULL, serve, &context) != 0) {
        perror("pthread_create");
        close(sockets[0]);
        close(sockets[1]);
        return 1;
    }

    int failed = 0;
    struct aicp_header hello =
        aicp_make_header(AICP_MSG_HELLO, 0, 0, 1, 1000, AICP_OK);
    failed |= aicp_posix_send_frame(sockets[0], hello, NULL) != 0;

    struct aicp_status_payload hello_status;
    failed |= expect_reply(sockets[0], AICP_MSG_STATUS, 1, AICP_OK, &hello_status);
    if (hello_status.applied_seq != 0) {
        fprintf(stderr, "HELLO reply unexpectedly reports applied_seq=%u\n", hello_status.applied_seq);
        failed = 1;
    }

    struct aicp_status_payload first_status;
    failed |= send_control(sockets[0], 2, 0.75f) != 0;
    failed |= expect_reply(sockets[0], AICP_MSG_STATUS, 2, AICP_OK, &first_status);

    struct aicp_status_payload duplicate_status;
    failed |= send_control(sockets[0], 2, 0.10f) != 0;
    failed |= expect_reply(sockets[0], AICP_MSG_STATUS, 2, AICP_OK, &duplicate_status);
    if (first_status.applied_seq != 2 || duplicate_status.applied_seq != 2 ||
        fabsf(first_status.setpoint - duplicate_status.setpoint) > 0.0001f) {
        fprintf(stderr, "duplicate request changed the control state\n");
        failed = 1;
    }

    struct aicp_header stale =
        aicp_make_header(AICP_MSG_HEARTBEAT, 0, 0, 1, 2000, AICP_OK);
    failed |= aicp_posix_send_frame(sockets[0], stale, NULL) != 0;
    failed |= expect_reply(sockets[0], AICP_MSG_ERROR, 1, AICP_ERR_SEQUENCE, NULL);

    const uint8_t short_payload = 0x5a;
    struct aicp_header bad_payload = aicp_make_header(
        AICP_MSG_CONTROL_SET, 0, sizeof(short_payload), 3, 3000, AICP_OK);
    failed |= aicp_posix_send_frame(sockets[0], bad_payload, &short_payload) != 0;
    failed |= expect_reply(sockets[0], AICP_MSG_ERROR, 3, AICP_ERR_BAD_PAYLOAD, NULL);
    failed |= aicp_posix_send_frame(sockets[0], bad_payload, &short_payload) != 0;
    failed |= expect_reply(sockets[0], AICP_MSG_ERROR, 3, AICP_ERR_BAD_PAYLOAD, NULL);

    const struct aicp_control_payload non_finite_control = {
        .target = NAN,
        .kp = 0.65f,
        .ki = 0.08f,
        .kd = 0.03f,
        .feed_forward = 0.05f,
        .mode = 1,
    };
    failed |= send_control_payload(sockets[0], 4, &non_finite_control) != 0;
    failed |= expect_reply(sockets[0], AICP_MSG_ERROR, 4, AICP_ERR_BAD_PAYLOAD, NULL);
    failed |= send_control_payload(sockets[0], 4, &non_finite_control) != 0;
    failed |= expect_reply(sockets[0], AICP_MSG_ERROR, 4, AICP_ERR_BAD_PAYLOAD, NULL);

    const struct aicp_control_payload positive_infinity_control = {
        .target = 0.6f,
        .kp = INFINITY,
        .ki = 0.08f,
        .kd = 0.03f,
        .feed_forward = 0.05f,
        .mode = 1,
    };
    failed |= send_control_payload(sockets[0], 5, &positive_infinity_control) != 0;
    failed |= expect_reply(sockets[0], AICP_MSG_ERROR, 5, AICP_ERR_BAD_PAYLOAD, NULL);

    const struct aicp_control_payload negative_infinity_control = {
        .target = 0.6f,
        .kp = 0.65f,
        .ki = 0.08f,
        .kd = 0.03f,
        .feed_forward = -INFINITY,
        .mode = 1,
    };
    failed |= send_control_payload(sockets[0], 6, &negative_infinity_control) != 0;
    failed |= expect_reply(sockets[0], AICP_MSG_ERROR, 6, AICP_ERR_BAD_PAYLOAD, NULL);

    const struct aicp_control_payload out_of_range_control = {
        .target = 1.01f,
        .kp = 0.65f,
        .ki = 0.08f,
        .kd = 0.03f,
        .feed_forward = 0.05f,
        .mode = 2,
    };
    failed |= send_control_payload(sockets[0], 7, &out_of_range_control) != 0;
    failed |= expect_reply(sockets[0], AICP_MSG_ERROR, 7, AICP_ERR_BAD_PAYLOAD, NULL);

    struct aicp_status_payload recovery_status;
    failed |= send_control(sockets[0], 8, 0.60f) != 0;
    failed |= expect_reply(sockets[0], AICP_MSG_STATUS, 8, AICP_OK, &recovery_status);
    if (recovery_status.applied_seq != 8 || !isfinite(recovery_status.setpoint) ||
        fabsf(recovery_status.setpoint - 0.60f) > 0.0001f) {
        fprintf(stderr, "invalid control request poisoned the control state\n");
        failed = 1;
    }

    struct aicp_header bad_type = aicp_make_header(0x7f, 0, 0, 9, 6000, AICP_OK);
    failed |= aicp_posix_send_frame(sockets[0], bad_type, NULL) != 0;
    failed |= expect_reply(sockets[0], AICP_MSG_ERROR, 9, AICP_ERR_BAD_TYPE, NULL);
    failed |= aicp_posix_send_frame(sockets[0], bad_type, NULL) != 0;
    failed |= expect_reply(sockets[0], AICP_MSG_ERROR, 9, AICP_ERR_BAD_TYPE, NULL);

    struct aicp_header bad_version =
        aicp_make_header(AICP_MSG_HEARTBEAT, 0, 0, 10, 7000, AICP_OK);
    bad_version.version = AICP_VERSION + 1;
    failed |= write_raw_frame(sockets[0], bad_version, NULL) != 0;
    failed |= expect_reply(sockets[0], AICP_MSG_ERROR, 10, AICP_ERR_VERSION, NULL);

    close(sockets[0]);
    if (pthread_join(thread, NULL) != 0) {
        perror("pthread_join");
        failed = 1;
    }
    close(sockets[1]);

    if (context.result != -ECONNRESET || context.stats.received_frames != 15 ||
        context.stats.control_requests != 2 || context.stats.protocol_errors != 8 ||
        context.stats.duplicate_requests != 4 || context.stats.stale_requests != 1) {
        fprintf(
            stderr,
            "unexpected service summary: result=%d frames=%u control=%u protocol=%u "
            "duplicate=%u stale=%u\n",
            context.result,
            context.stats.received_frames,
            context.stats.control_requests,
            context.stats.protocol_errors,
            context.stats.duplicate_requests,
            context.stats.stale_requests);
        failed = 1;
    }
    if (context.events[AICP_SERVICE_HELLO] != 1 ||
        context.events[AICP_SERVICE_CONTROL_APPLIED] != 2 ||
        context.events[AICP_SERVICE_STATUS_SENT] != 4 ||
        context.events[AICP_SERVICE_ERROR_SENT] != 11 ||
        context.events[AICP_SERVICE_DUPLICATE] != 4 ||
        context.events[AICP_SERVICE_STALE] != 1 ||
        context.events[AICP_SERVICE_DISCONNECTED] != 1) {
        fprintf(stderr, "unexpected service event counters\n");
        failed = 1;
    }

    return failed;
}

int main(void) {
    int failed = run_service_sequence_test();
    printf("AICP_SERVICE_SUMMARY passed=%d failed=%d\n", failed == 0, failed != 0);
    return failed == 0 ? 0 : 1;
}
