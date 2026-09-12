// Copyright 2026 The TGOSKits Authors
//
// SPDX-License-Identifier: Apache-2.0

#include "aicp_datagram.h"
#include "aicp_posix_stream.h"

#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <sys/wait.h>

struct write_counter {
    unsigned calls;
};

static ptrdiff_t count_writes(void *context, const void *buffer, size_t length) {
    struct write_counter *counter = context;

    (void)buffer;
    counter->calls++;
    return (ptrdiff_t)length;
}

static int make_pair(int fds[2]) {
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, fds) != 0) {
        perror("socketpair");
        return 1;
    }
    return 0;
}

static void close_pair(int fds[2]) {
    close(fds[0]);
    close(fds[1]);
}

static int write_raw_frame(int fd, struct aicp_header hdr, const void *payload) {
    uint8_t wire[AICP_HEADER_LEN];

    hdr.crc16 = aicp_frame_crc(hdr, payload);
    aicp_header_encode(&hdr, wire);
    int ret = aicp_posix_write_full(fd, wire, sizeof(wire));
    if (ret != 0) {
        return ret;
    }
    if (hdr.payload_len != 0) {
        return aicp_posix_write_full(fd, payload, hdr.payload_len);
    }
    return 0;
}

static struct aicp_header raw_header(
    uint8_t version,
    uint8_t msg_type,
    uint32_t payload_len,
    uint32_t seq,
    uint16_t error_code) {
    struct aicp_header hdr = aicp_make_header(
        msg_type, 0, payload_len, seq, 1234 + seq, error_code);
    hdr.version = version;
    return hdr;
}

static int expect_recv_error(int fd, int expected, const char *name) {
    uint8_t rx[AICP_MAX_PAYLOAD];
    struct aicp_header out;
    int ret = aicp_posix_recv_frame(fd, &out, rx, sizeof(rx));
    if (ret != expected) {
        fprintf(stderr, "%s: expected %d, got %d\n", name, expected, ret);
        return 1;
    }
    return 0;
}

static int send_error_reply(int fd, const struct aicp_header *request, uint16_t code) {
    const char payload[] = "{\"error\":\"invalid AICP frame\"}";
    struct aicp_header reply = aicp_make_header(
        AICP_MSG_ERROR, 0, (uint32_t)sizeof(payload), request->seq, 9876, code);
    return aicp_posix_send_frame(fd, reply, payload);
}

static int read_error_reply(int fd, uint32_t seq, uint16_t code, const char *name) {
    uint8_t payload[AICP_MAX_PAYLOAD];
    struct aicp_header hdr;
    int ret = aicp_posix_recv_frame(fd, &hdr, payload, sizeof(payload));
    if (ret != 0) {
        fprintf(stderr, "%s: failed to read error reply: %d\n", name, ret);
        return 1;
    }
    if (hdr.msg_type != AICP_MSG_ERROR || hdr.seq != seq || hdr.error_code != code) {
        fprintf(stderr,
                "%s: unexpected reply type=%u seq=%u error=%u\n",
                name,
                hdr.msg_type,
                hdr.seq,
                hdr.error_code);
        return 1;
    }
    return 0;
}

static int test_round_trip_frame(void) {
    int fds[2];
    if (make_pair(fds) != 0) {
        return 1;
    }

    struct aicp_control_payload payload = {
        .target = 0.7f,
        .kp = 0.6f,
        .ki = 0.05f,
        .kd = 0.02f,
        .feed_forward = 0.1f,
        .mode = 1,
    };
    uint8_t payload_wire[AICP_CONTROL_PAYLOAD_LEN];
    aicp_control_payload_encode(&payload, payload_wire);
    struct aicp_header hdr = aicp_make_header(
        AICP_MSG_CONTROL_SET,
        0,
        AICP_CONTROL_PAYLOAD_LEN,
        31,
        5678,
        AICP_OK);
    if (aicp_posix_send_frame(fds[0], hdr, payload_wire) != 0) {
        perror("aicp_send_frame");
        close_pair(fds);
        return 1;
    }

    uint8_t rx[AICP_MAX_PAYLOAD];
    struct aicp_header out;
    int ret = aicp_posix_recv_frame(fds[1], &out, rx, sizeof(rx));
    close_pair(fds);
    if (ret != 0) {
        fprintf(stderr, "recv failed: %d\n", ret);
        return 1;
    }
    if (out.seq != 31 || out.msg_type != AICP_MSG_CONTROL_SET ||
        out.payload_len != AICP_CONTROL_PAYLOAD_LEN) {
        fprintf(stderr, "decoded header mismatch\n");
        return 1;
    }
    if (memcmp(rx, payload_wire, sizeof(payload_wire)) != 0) {
        fprintf(stderr, "payload mismatch\n");
        return 1;
    }
    return 0;
}

static int test_crc_detects_corruption(void) {
    int fds[2];
    if (make_pair(fds) != 0) {
        return 1;
    }

    const char payload[] = "hello";
    struct aicp_header hdr = aicp_make_header(
        AICP_MSG_HELLO, 0, (uint32_t)sizeof(payload), 17, 1234, AICP_OK);
    if (aicp_posix_send_frame(fds[0], hdr, payload) != 0) {
        perror("aicp_send_frame");
        close_pair(fds);
        return 1;
    }

    uint8_t wire[AICP_HEADER_LEN + sizeof(payload)];
    ssize_t n = read(fds[1], wire, sizeof(wire));
    if (n != (ssize_t)sizeof(wire)) {
        fprintf(stderr, "short socketpair read: %zd\n", n);
        close_pair(fds);
        return 1;
    }
    wire[sizeof(wire) - 1] ^= 0x01u;
    if (write(fds[1], wire, sizeof(wire)) != (ssize_t)sizeof(wire)) {
        perror("write corrupted frame");
        close_pair(fds);
        return 1;
    }

    int failed = expect_recv_error(fds[0], -EBADMSG, "crc corruption");
    close_pair(fds);
    return failed;
}

static int test_rejects_bad_magic(void) {
    int fds[2];
    if (make_pair(fds) != 0) {
        return 1;
    }

    struct aicp_header hdr = raw_header(AICP_VERSION, AICP_MSG_HELLO, 0, 41, AICP_OK);
    hdr.magic = 0xbeefu;
    if (write_raw_frame(fds[0], hdr, NULL) != 0) {
        close_pair(fds);
        return 1;
    }
    int failed = expect_recv_error(fds[1], -EPROTO, "bad magic");
    close_pair(fds);
    return failed;
}

static int test_rejects_unsupported_header_options(void) {
    int fds[2];
    if (make_pair(fds) != 0) {
        return 1;
    }

    struct aicp_header hdr = raw_header(AICP_VERSION, AICP_MSG_HELLO, 0, 42, AICP_OK);
    hdr.flags = 1;
    if (write_raw_frame(fds[0], hdr, NULL) != 0) {
        close_pair(fds);
        return 1;
    }
    int failed = expect_recv_error(fds[1], -EPROTO, "unsupported header options");
    close_pair(fds);
    return failed;
}

static int test_rejects_out_of_range_control_payload(void) {
    struct aicp_control_payload control = {
        .target = 0.5f,
        .kp = 0.7f,
        .ki = 0.1f,
        .kd = 0.02f,
        .feed_forward = 0.03f,
        .mode = 1,
    };
    if (!aicp_control_payload_is_valid(&control)) {
        return 1;
    }
    control.mode = 2;
    if (aicp_control_payload_is_valid(&control)) {
        return 1;
    }
    control.mode = 1;
    control.target = 1.01f;
    return aicp_control_payload_is_valid(&control) ? 1 : 0;
}

static int test_rejects_oversized_payload_before_reading_body(void) {
    int fds[2];
    if (make_pair(fds) != 0) {
        return 1;
    }

    uint8_t wire[AICP_HEADER_LEN];
    struct aicp_header hdr = raw_header(
        AICP_VERSION, AICP_MSG_CONTROL_SET, AICP_MAX_PAYLOAD + 1u, 42, AICP_OK);
    hdr.crc16 = aicp_frame_crc(hdr, NULL);
    aicp_header_encode(&hdr, wire);
    if (aicp_posix_write_full(fds[0], wire, sizeof(wire)) != 0) {
        close_pair(fds);
        return 1;
    }
    int failed = expect_recv_error(fds[1], -EMSGSIZE, "oversized payload");
    close_pair(fds);
    return failed;
}

static int test_send_rejects_oversized_payload_before_crc(void) {
    struct write_counter counter = {0};
    struct aicp_stream stream = {
        .write = count_writes,
        .context = &counter,
    };
    uint8_t payload[AICP_MAX_PAYLOAD + 1u] = {0};
    const struct aicp_header header = aicp_make_header(
        AICP_MSG_CONTROL_SET,
        0,
        sizeof(payload),
        43,
        1234,
        AICP_OK);

    const int result = aicp_stream_send_frame(&stream, header, payload);
    if (result != -EMSGSIZE || counter.calls != 0) {
        fprintf(stderr,
                "oversized send: expected -EMSGSIZE without writes, got result=%d calls=%u\n",
                result,
                counter.calls);
        return 1;
    }
    return 0;
}

static int test_peer_close_write_returns_epipe_without_sigpipe(void) {
    int fds[2];
    int ready[2];
    if (make_pair(fds) != 0) {
        return 1;
    }
    if (pipe(ready) != 0) {
        perror("pipe");
        close_pair(fds);
        return 1;
    }

    const pid_t child = fork();
    if (child < 0) {
        perror("fork");
        close_pair(fds);
        close(ready[0]);
        close(ready[1]);
        return 1;
    }
    if (child == 0) {
        const uint8_t payload = 0x5a;
        uint8_t release;

        close(fds[1]);
        close(ready[1]);
        if (read(ready[0], &release, sizeof(release)) != (ssize_t)sizeof(release)) {
            close(ready[0]);
            close(fds[0]);
            _exit(1);
        }
        close(ready[0]);
        const int result = aicp_posix_write_full(fds[0], &payload, sizeof(payload));
        close(fds[0]);
        _exit(result == -EPIPE ? 0 : 1);
    }

    close_pair(fds);
    close(ready[0]);
    const uint8_t release = 1;
    if (write(ready[1], &release, sizeof(release)) != (ssize_t)sizeof(release)) {
        perror("write");
        close(ready[1]);
        (void)waitpid(child, NULL, 0);
        return 1;
    }
    close(ready[1]);
    int status = 0;
    if (waitpid(child, &status, 0) != child) {
        perror("waitpid");
        return 1;
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fprintf(stderr, "closed peer write did not return -EPIPE safely: status=%d\n", status);
        return 1;
    }
    return 0;
}

static int test_version_error_reply(void) {
    int fds[2];
    if (make_pair(fds) != 0) {
        return 1;
    }

    struct aicp_header req = raw_header(99, AICP_MSG_HEARTBEAT, 0, 51, AICP_OK);
    if (write_raw_frame(fds[0], req, NULL) != 0) {
        close_pair(fds);
        return 1;
    }

    uint8_t payload[AICP_MAX_PAYLOAD];
    struct aicp_header decoded;
    int ret = aicp_posix_recv_frame(fds[1], &decoded, payload, sizeof(payload));
    if (ret != 0 || decoded.version == AICP_VERSION) {
        fprintf(stderr, "version test decode failed ret=%d version=%u\n", ret, decoded.version);
        close_pair(fds);
        return 1;
    }
    if (send_error_reply(fds[1], &decoded, AICP_ERR_VERSION) != 0) {
        close_pair(fds);
        return 1;
    }
    int failed = read_error_reply(fds[0], 51, AICP_ERR_VERSION, "version error");
    close_pair(fds);
    return failed;
}

static int test_bad_type_error_reply(void) {
    int fds[2];
    if (make_pair(fds) != 0) {
        return 1;
    }

    struct aicp_header req = raw_header(AICP_VERSION, 0xfeu, 0, 52, AICP_OK);
    if (write_raw_frame(fds[0], req, NULL) != 0) {
        close_pair(fds);
        return 1;
    }

    uint8_t payload[AICP_MAX_PAYLOAD];
    struct aicp_header decoded;
    int ret = aicp_posix_recv_frame(fds[1], &decoded, payload, sizeof(payload));
    if (ret != 0 || decoded.msg_type != 0xfeu) {
        fprintf(stderr, "bad type decode failed ret=%d type=%u\n", ret, decoded.msg_type);
        close_pair(fds);
        return 1;
    }
    if (send_error_reply(fds[1], &decoded, AICP_ERR_BAD_TYPE) != 0) {
        close_pair(fds);
        return 1;
    }
    int failed = read_error_reply(fds[0], 52, AICP_ERR_BAD_TYPE, "bad type error");
    close_pair(fds);
    return failed;
}

static int test_bad_payload_error_reply(void) {
    int fds[2];
    if (make_pair(fds) != 0) {
        return 1;
    }

    const char payload[] = "short";
    struct aicp_header req = raw_header(
        AICP_VERSION, AICP_MSG_CONTROL_SET, (uint32_t)sizeof(payload), 53, AICP_OK);
    if (write_raw_frame(fds[0], req, payload) != 0) {
        close_pair(fds);
        return 1;
    }

    uint8_t decoded_payload[AICP_MAX_PAYLOAD];
    struct aicp_header decoded = {0};
    int ret = aicp_posix_recv_frame(
        fds[1], &decoded, decoded_payload, sizeof(decoded_payload));
    if (ret != 0 || decoded.payload_len == AICP_CONTROL_PAYLOAD_LEN) {
        fprintf(stderr, "bad payload decode failed ret=%d len=%u\n", ret, decoded.payload_len);
        close_pair(fds);
        return 1;
    }
    if (send_error_reply(fds[1], &decoded, AICP_ERR_BAD_PAYLOAD) != 0) {
        close_pair(fds);
        return 1;
    }
    int failed = read_error_reply(fds[0], 53, AICP_ERR_BAD_PAYLOAD, "bad payload error");
    close_pair(fds);
    return failed;
}

static int test_timeout_is_reported(void) {
    int fds[2];
    if (make_pair(fds) != 0) {
        return 1;
    }

    struct timeval tv = {
        .tv_sec = 0,
        .tv_usec = 100000,
    };
    setsockopt(fds[0], SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));

    uint8_t payload[AICP_MAX_PAYLOAD];
    struct aicp_header hdr;
    int ret = aicp_posix_recv_frame(fds[0], &hdr, payload, sizeof(payload));
    close_pair(fds);
    if (ret != -EAGAIN && ret != -EWOULDBLOCK) {
        fprintf(stderr, "timeout: expected -EAGAIN/-EWOULDBLOCK, got %d\n", ret);
        return 1;
    }
    return 0;
}

static int test_reconnect_after_peer_close(void) {
    int fds[2];
    if (make_pair(fds) != 0) {
        return 1;
    }

    close(fds[1]);
    if (expect_recv_error(fds[0], -ECONNRESET, "peer close") != 0) {
        close(fds[0]);
        return 1;
    }
    close(fds[0]);

    if (make_pair(fds) != 0) {
        return 1;
    }
    const char payload[] = "reconnected";
    struct aicp_header hdr = aicp_make_header(
        AICP_MSG_HELLO, 0, (uint32_t)sizeof(payload), 61, 1111, AICP_OK);
    if (aicp_posix_send_frame(fds[0], hdr, payload) != 0) {
        close_pair(fds);
        return 1;
    }

    uint8_t rx[AICP_MAX_PAYLOAD];
    struct aicp_header out;
    int ret = aicp_posix_recv_frame(fds[1], &out, rx, sizeof(rx));
    close_pair(fds);
    if (ret != 0 || out.seq != 61 || memcmp(rx, payload, sizeof(payload)) != 0) {
        fprintf(stderr, "reconnect read failed ret=%d seq=%u\n", ret, out.seq);
        return 1;
    }
    return 0;
}

static int test_datagram_round_trip(void) {
    const char payload[] = "udp-control";
    uint8_t packet[AICP_HEADER_LEN + AICP_MAX_PAYLOAD];
    size_t packet_len = 0;
    const struct aicp_header header = aicp_make_header(
        AICP_MSG_CONTROL_SET,
        0,
        (uint32_t)sizeof(payload),
        71,
        1234,
        AICP_OK);

    int ret = aicp_datagram_encode(
        header, payload, packet, sizeof(packet), &packet_len);
    if (ret != 0) {
        fprintf(stderr, "datagram encode failed ret=%d\n", ret);
        return 1;
    }

    char decoded_payload[sizeof(payload)];
    struct aicp_header decoded;
    ret = aicp_datagram_decode(
        packet,
        packet_len,
        &decoded,
        decoded_payload,
        sizeof(decoded_payload));
    if (ret != 0 || decoded.seq != header.seq ||
        memcmp(decoded_payload, payload, sizeof(payload)) != 0) {
        fprintf(stderr, "datagram round trip failed ret=%d seq=%u\n", ret, decoded.seq);
        return 1;
    }
    return 0;
}

static int test_datagram_rejects_unsupported_version(void) {
    const uint8_t payload[] = {0x11, 0x22, 0x33, 0x44};
    uint8_t packet[AICP_HEADER_LEN + sizeof(payload)];
    struct aicp_header header = aicp_make_header(
        AICP_MSG_STATUS,
        0,
        sizeof(payload),
        72,
        1234,
        AICP_OK);
    header.version = AICP_VERSION + 1;
    header.crc16 = aicp_frame_crc(header, payload);
    aicp_header_encode(&header, packet);
    memcpy(packet + AICP_HEADER_LEN, payload, sizeof(payload));

    uint8_t decoded_payload[sizeof(payload)] = {0xaa, 0xbb, 0xcc, 0xdd};
    const uint8_t original_payload[sizeof(decoded_payload)] = {0xaa, 0xbb, 0xcc, 0xdd};
    struct aicp_header decoded = {0};
    int ret = aicp_datagram_decode(
        packet,
        sizeof(packet),
        &decoded,
        decoded_payload,
        sizeof(decoded_payload));
    if (ret != -EPROTO ||
        memcmp(decoded_payload, original_payload, sizeof(decoded_payload)) != 0) {
        fprintf(stderr, "datagram version: expected -EPROTO without payload write, got %d\n", ret);
        return 1;
    }
    return 0;
}

static int test_datagram_rejects_trailing_bytes(void) {
    uint8_t packet[AICP_HEADER_LEN + 2];
    size_t packet_len = 0;
    const uint8_t payload = 0x5a;
    const struct aicp_header header = aicp_make_header(
        AICP_MSG_HEARTBEAT, 0, sizeof(payload), 72, 1234, AICP_OK);

    int ret = aicp_datagram_encode(
        header, &payload, packet, sizeof(packet), &packet_len);
    if (ret != 0) {
        return 1;
    }
    packet[packet_len] = 0;

    uint8_t decoded_payload = 0;
    struct aicp_header decoded;
    ret = aicp_datagram_decode(
        packet,
        packet_len + 1,
        &decoded,
        &decoded_payload,
        sizeof(decoded_payload));
    if (ret != -EBADMSG) {
        fprintf(stderr, "datagram trailing bytes: expected -EBADMSG, got %d\n", ret);
        return 1;
    }
    return 0;
}

static int test_datagram_crc_detects_corruption(void) {
    uint8_t packet[AICP_HEADER_LEN + 1];
    size_t packet_len = 0;
    const uint8_t payload = 0x5a;
    const struct aicp_header header = aicp_make_header(
        AICP_MSG_HEARTBEAT, 0, sizeof(payload), 73, 1234, AICP_OK);

    int ret = aicp_datagram_encode(
        header, &payload, packet, sizeof(packet), &packet_len);
    if (ret != 0) {
        return 1;
    }
    packet[AICP_HEADER_LEN] ^= 0xffu;

    uint8_t decoded_payload = 0;
    struct aicp_header decoded;
    ret = aicp_datagram_decode(
        packet,
        packet_len,
        &decoded,
        &decoded_payload,
        sizeof(decoded_payload));
    if (ret != -EBADMSG) {
        fprintf(stderr, "datagram crc: expected -EBADMSG, got %d\n", ret);
        return 1;
    }
    return 0;
}

struct test_case {
    const char *name;
    int (*run)(void);
};

int main(void) {
    const struct test_case tests[] = {
        {"round_trip_frame", test_round_trip_frame},
        {"crc_detects_corruption", test_crc_detects_corruption},
        {"rejects_bad_magic", test_rejects_bad_magic},
        {"rejects_unsupported_header_options", test_rejects_unsupported_header_options},
        {"rejects_out_of_range_control_payload", test_rejects_out_of_range_control_payload},
        {"rejects_oversized_payload", test_rejects_oversized_payload_before_reading_body},
        {"send_rejects_oversized_payload", test_send_rejects_oversized_payload_before_crc},
        {"peer_close_write_returns_epipe", test_peer_close_write_returns_epipe_without_sigpipe},
        {"version_error_reply", test_version_error_reply},
        {"bad_type_error_reply", test_bad_type_error_reply},
        {"bad_payload_error_reply", test_bad_payload_error_reply},
        {"timeout_is_reported", test_timeout_is_reported},
        {"reconnect_after_peer_close", test_reconnect_after_peer_close},
        {"datagram_round_trip", test_datagram_round_trip},
        {"datagram_rejects_unsupported_version", test_datagram_rejects_unsupported_version},
        {"datagram_rejects_trailing_bytes", test_datagram_rejects_trailing_bytes},
        {"datagram_crc_detects_corruption", test_datagram_crc_detects_corruption},
    };

    unsigned passed = 0;
    for (size_t i = 0; i < sizeof(tests) / sizeof(tests[0]); i++) {
        int ret = tests[i].run();
        printf("AICP_PROTOCOL_TEST name=%s result=%s\n", tests[i].name, ret == 0 ? "PASS" : "FAIL");
        if (ret != 0) {
            return 1;
        }
        passed++;
    }

    printf("AICP_PROTOCOL_SUMMARY passed=%u failed=0\n", passed);
    return 0;
}
