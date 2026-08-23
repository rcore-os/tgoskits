// SPDX-License-Identifier: Apache-2.0
// Minimal POSIX StarryOS client for the StarryOS -> ArceOS GIPC control path.

#define _POSIX_C_SOURCE 200809L

#include <arpa/inet.h>
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

#define GIPC_MAGIC 0x47495043u
#define GIPC_VERSION 1u
#define GIPC_HEADER_LEN 32u
#define GIPC_MAX_PAYLOAD 1200u
#define GIPC_CONTROL 2u
#define GIPC_STATUS 3u
#define GIPC_ERROR 4u
#define GIPC_PORT 4242u

struct gipc_header {
    uint32_t magic;
    uint8_t version;
    uint8_t message_type;
    uint16_t flags;
    uint16_t header_len;
    uint16_t payload_len;
    uint32_t sequence;
    uint64_t timestamp_ns;
    uint16_t error_code;
    uint32_t checksum;
};

static uint32_t crc32(const uint8_t *data, size_t length) {
    uint32_t crc = 0xffffffffu;
    for (size_t i = 0; i < length; i++) {
        crc ^= data[i];
        for (unsigned bit = 0; bit < 8; bit++) {
            crc = (crc & 1u) ? (crc >> 1) ^ 0xedb88320u : crc >> 1;
        }
    }
    return ~crc;
}

static uint64_t monotonic_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ull + (uint64_t)ts.tv_nsec;
}

static int write_full(int fd, const void *buffer, size_t length) {
    const uint8_t *cursor = buffer;
    while (length != 0) {
        ssize_t written = send(fd, cursor, length, 0);
        if (written <= 0) return -errno;
        cursor += (size_t)written;
        length -= (size_t)written;
    }
    return 0;
}

static int read_full(int fd, void *buffer, size_t length) {
    uint8_t *cursor = buffer;
    while (length != 0) {
        ssize_t received = recv(fd, cursor, length, 0);
        if (received == 0) return -ECONNRESET;
        if (received < 0) return -errno;
        cursor += (size_t)received;
        length -= (size_t)received;
    }
    return 0;
}

static int connect_peer(const char *address) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) return -1;
    struct timeval timeout = {.tv_sec = 1, .tv_usec = 0};
    if (setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof(timeout)) != 0 ||
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout)) != 0) {
        close(fd);
        return -1;
    }
    struct sockaddr_in peer = {.sin_family = AF_INET, .sin_port = htons(GIPC_PORT)};
    if (inet_pton(AF_INET, address, &peer.sin_addr) != 1 ||
        connect(fd, (struct sockaddr *)&peer, sizeof(peer)) != 0) {
        close(fd);
        return -1;
    }
    return fd;
}

static void put_u16(uint8_t *p, uint16_t value) { uint16_t wire = htons(value); memcpy(p, &wire, 2); }
static void put_u32(uint8_t *p, uint32_t value) { uint32_t wire = htonl(value); memcpy(p, &wire, 4); }
static uint16_t get_u16(const uint8_t *p) { uint16_t wire; memcpy(&wire, p, 2); return ntohs(wire); }
static uint32_t get_u32(const uint8_t *p) { uint32_t wire; memcpy(&wire, p, 4); return ntohl(wire); }

static int verify_crc(const uint8_t *frame, size_t length) {
    if (length < GIPC_HEADER_LEN) return 0;
    uint8_t copy[GIPC_HEADER_LEN + GIPC_MAX_PAYLOAD];
    if (length > sizeof(copy)) return 0;
    memcpy(copy, frame, length);
    uint32_t expected = get_u32(copy + 26);
    memset(copy + 26, 0, 4);
    return expected == crc32(copy, length);
}

static size_t encode_control(uint8_t *frame, uint32_t sequence, uint64_t timestamp) {
    const uint8_t payload[8] = {0, 0, 0, 1, 0, 0, 0, 0};
    memset(frame, 0, GIPC_HEADER_LEN + sizeof(payload));
    put_u32(frame + 0, GIPC_MAGIC);
    frame[4] = GIPC_VERSION;
    frame[5] = GIPC_CONTROL;
    put_u16(frame + 6, 1u); // ACK_REQUIRED
    put_u16(frame + 8, GIPC_HEADER_LEN);
    put_u16(frame + 10, sizeof(payload));
    put_u32(frame + 12, sequence);
    put_u32(frame + 16, (uint32_t)(timestamp >> 32));
    put_u32(frame + 20, (uint32_t)timestamp);
    memcpy(frame + GIPC_HEADER_LEN, payload, sizeof(payload));
    put_u32(frame + 26, crc32(frame, GIPC_HEADER_LEN + sizeof(payload)));
    return GIPC_HEADER_LEN + sizeof(payload);
}

int main(int argc, char **argv) {
    const char *address = argc > 1 ? argv[1] : "10.0.42.2";
    uint8_t frame[GIPC_HEADER_LEN + GIPC_MAX_PAYLOAD];
    unsigned timeouts = 0;
    uint64_t request_start = monotonic_ns();
    for (unsigned attempt = 0; attempt < 3; attempt++) {
        int fd = connect_peer(address);
        if (fd < 0) { timeouts++; continue; }
        size_t frame_len = encode_control(frame, 1, monotonic_ns());
        if (write_full(fd, frame, frame_len) != 0) {
            close(fd); timeouts++; continue;
        }
        uint8_t response[GIPC_HEADER_LEN + GIPC_MAX_PAYLOAD];
        if (read_full(fd, response, GIPC_HEADER_LEN) != 0) {
            close(fd); timeouts++; continue;
        }
        uint16_t payload_len = get_u16(response + 10);
        if (payload_len > GIPC_MAX_PAYLOAD ||
            read_full(fd, response + GIPC_HEADER_LEN, payload_len) != 0) {
            close(fd); timeouts++; continue;
        }
        if (get_u32(response) != GIPC_MAGIC || response[4] != GIPC_VERSION ||
            response[5] != GIPC_STATUS || get_u32(response + 12) != 1 ||
            !verify_crc(response, GIPC_HEADER_LEN + payload_len)) {
            close(fd); fprintf(stderr, "unexpected response type or sequence\n"); return 1;
        }
        uint64_t elapsed_ns = monotonic_ns() - request_start;
        if (elapsed_ns == 0) elapsed_ns = 1;
        printf("GIPC_STARRY_STATUS seq=%u payload=%u attempts=%u timeouts=%u\n",
               get_u32(response + 12), payload_len, attempt + 1, timeouts);
        printf("GIPC_STARRY_METRIC success=1 errors=0 timeouts=%u rtt_ns=%llu throughput_bps=%llu\n",
               timeouts,
               (unsigned long long)elapsed_ns,
               (unsigned long long)((payload_len * 1000000000ull) / elapsed_ns));
        close(fd);
        return 0;
    }
    fprintf(stderr, "GIPC_STARRY_TIMEOUT attempts=3\n");
    return 1;
}
