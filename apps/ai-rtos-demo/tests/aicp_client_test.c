// Copyright 2026 The TGOSKits Authors
//
// SPDX-License-Identifier: Apache-2.0

#include "aicp_client.h"
#include "aicp_posix_stream.h"

#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

struct server_case {
    int socket;
    int corrupt_response_seq;
    int corrupt_response_version;
    int result;
};

struct client_context {
    uint64_t now;
    unsigned tx_begin;
    unsigned tx_complete;
    unsigned rx_complete;
};

static uint64_t fake_monotonic_ns(void *context) {
    struct client_context *client = context;
    client->now += 1000;
    return client->now;
}

static void record_event(
    void *context,
    const struct aicp_client_event *event) {
    struct client_context *trace = context;
    switch (event->kind) {
    case AICP_CLIENT_TX_BEGIN:
        trace->tx_begin++;
        break;
    case AICP_CLIENT_TX_COMPLETE:
        trace->tx_complete++;
        break;
    case AICP_CLIENT_RX_COMPLETE:
        trace->rx_complete++;
        break;
    }
}

static int send_response(
    struct aicp_stream *stream,
    struct aicp_header response,
    const struct aicp_status_payload *status) {
    uint8_t wire[AICP_HEADER_LEN];
    uint8_t status_wire[AICP_STATUS_PAYLOAD_LEN];

    response.magic = AICP_MAGIC;
    response.header_len = AICP_HEADER_LEN;
    aicp_status_payload_encode(status, status_wire);
    response.crc16 = aicp_frame_crc(response, status_wire);
    aicp_header_encode(&response, wire);

    int result = aicp_stream_write_full(stream, wire, sizeof(wire));
    if (result != 0) {
        return result;
    }
    return aicp_stream_write_full(stream, status_wire, sizeof(status_wire));
}

static void *serve_client(void *argument) {
    struct server_case *test = argument;
    struct aicp_posix_stream stream;
    uint8_t payload[AICP_MAX_PAYLOAD];
    struct aicp_header request;

    aicp_posix_stream_init(&stream, test->socket);

    test->result = aicp_stream_recv_frame(
        &stream.stream, &request, payload, sizeof(payload));
    if (test->result != 0 || request.msg_type != AICP_MSG_HELLO) {
        test->result = -1;
        return NULL;
    }

    const struct aicp_status_payload hello_status = {
        .setpoint = 0.0f,
        .measured = 0.0f,
        .control_output = 0.0f,
        .error = 0.0f,
        .mode = 0,
        .applied_seq = request.seq,
    };
    struct aicp_header hello_response = aicp_make_header(
        AICP_MSG_STATUS,
        0,
        AICP_STATUS_PAYLOAD_LEN,
        request.seq,
        1000,
        AICP_OK);
    test->result = send_response(&stream.stream, hello_response, &hello_status);
    if (test->result != 0) {
        return NULL;
    }

    test->result = aicp_stream_recv_frame(
        &stream.stream, &request, payload, sizeof(payload));
    if (test->result != 0 || request.msg_type != AICP_MSG_CONTROL_SET ||
        request.payload_len != AICP_CONTROL_PAYLOAD_LEN) {
        test->result = -1;
        return NULL;
    }
    static const uint8_t expected_control_wire[] = {
        0x3e, 0x80, 0x00, 0x00, 0x3f, 0x00, 0x00, 0x00,
        0x3d, 0xcc, 0xcc, 0xcd, 0x3c, 0x23, 0xd7, 0x0a,
        0x3e, 0x4c, 0xcc, 0xcd, 0x00, 0x00, 0x00, 0x01,
    };
    if (memcmp(payload, expected_control_wire, sizeof(expected_control_wire)) != 0) {
        test->result = -1;
        (void)shutdown(test->socket, SHUT_RDWR);
        return NULL;
    }

    const struct aicp_status_payload status = {
        .setpoint = 0.25f,
        .measured = 0.5f,
        .control_output = 0.75f,
        .error = -0.25f,
        .mode = 1,
        .applied_seq = request.seq,
    };
    const uint32_t response_seq =
        test->corrupt_response_seq ? request.seq + 1u : request.seq;
    struct aicp_header response = aicp_make_header(
        AICP_MSG_STATUS,
        0,
        AICP_STATUS_PAYLOAD_LEN,
        response_seq,
        1234,
        AICP_OK);
    if (test->corrupt_response_version) {
        response.version = AICP_VERSION + 1;
    }
    test->result = send_response(&stream.stream, response, &status);
    return NULL;
}

static int run_case(int corrupt_response_seq, int corrupt_response_version) {
    int sockets[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sockets) != 0) {
        return -1;
    }

    struct server_case server = {
        .socket = sockets[1],
        .corrupt_response_seq = corrupt_response_seq,
        .corrupt_response_version = corrupt_response_version,
        .result = 0,
    };
    pthread_t thread;
    if (pthread_create(&thread, NULL, serve_client, &server) != 0) {
        close(sockets[0]);
        close(sockets[1]);
        return -1;
    }

    struct client_context trace = {0};
    const struct aicp_client_ops ops = {
        .monotonic_ns = fake_monotonic_ns,
        .on_event = record_event,
        .context = &trace,
    };
    uint32_t seq = 1;
    const char hello[] = "{\"role\":\"client-test\"}";
    struct aicp_posix_stream stream;
    aicp_posix_stream_init(&stream, sockets[0]);
    struct aicp_status_payload hello_status;
    int result = aicp_client_session_handshake(
        &stream.stream, &seq, hello, sizeof(hello), &hello_status, &ops);

    const struct aicp_control_payload control = {
        .target = 0.25f,
        .kp = 0.5f,
        .ki = 0.1f,
        .kd = 0.01f,
        .feed_forward = 0.2f,
        .mode = 1,
    };
    struct aicp_status_payload status = {
        .setpoint = -1.0f,
        .measured = -2.0f,
        .control_output = -3.0f,
        .error = -4.0f,
        .mode = UINT32_MAX,
        .applied_seq = UINT32_MAX,
    };
    const struct aicp_status_payload original_status = status;
    uint64_t rtt_ns = 0;
    if (result == 0) {
        result = aicp_client_session_transact_control(
            &stream.stream, &seq, &control, &status, &rtt_ns, &ops);
    }

    close(sockets[0]);
    pthread_join(thread, NULL);
    close(sockets[1]);

    const int expect_protocol_error =
        corrupt_response_seq || corrupt_response_version;
    const int expected = expect_protocol_error ? -EPROTO : 0;
    if (result != expected || server.result != 0 || seq != 3 ||
        trace.tx_begin != 2 || trace.tx_complete != 2 ||
        trace.rx_complete != 2) {
        return -1;
    }
    if (hello_status.applied_seq != 1) {
        return -1;
    }
    if (expect_protocol_error &&
        memcmp(&status, &original_status, sizeof(status)) != 0) {
        return -1;
    }
    if (!expect_protocol_error && (status.applied_seq != 2 || rtt_ns != 1000)) {
        return -1;
    }
    return 0;
}

int main(void) {
    unsigned passed = 0;
    unsigned failed = 0;

    if (run_case(0, 0) == 0) {
        passed++;
    } else {
        failed++;
    }
    if (run_case(1, 0) == 0) {
        passed++;
    } else {
        failed++;
    }
    if (run_case(0, 1) == 0) {
        passed++;
    } else {
        failed++;
    }

    printf("AICP_CLIENT_SUMMARY passed=%u failed=%u\n", passed, failed);
    return failed == 0 ? 0 : 1;
}
