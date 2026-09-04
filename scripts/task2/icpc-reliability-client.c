#define _GNU_SOURCE
#include "icpc-wire.h"

#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <stdio.h>
#include <string.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

#define PEER_IP "10.0.9.3"
#define INITIAL_TIMEOUT_MS 50
#define MAX_TIMEOUT_MS 2000
#define MAX_RETRIES 20

static uint32_t total_retries;

static uint64_t monotonic_ms(void)
{
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0)
        return 0;
    return (uint64_t)ts.tv_sec * 1000 + (uint64_t)ts.tv_nsec / 1000000;
}

static uint32_t backoff_ms(unsigned attempt)
{
    uint32_t ms = INITIAL_TIMEOUT_MS << (attempt > 6 ? 6 : attempt);
    return ms > MAX_TIMEOUT_MS ? MAX_TIMEOUT_MS : ms;
}

static int send_frame(int fd, const struct sockaddr_in *peer, socklen_t peer_len,
                      uint8_t msg_type, uint8_t flags, uint32_t seq,
                      uint16_t err_code, const uint8_t *payload, size_t plen)
{
    uint8_t out[ICPC_MAX_FRAME];
    size_t n = icpc_encode(msg_type, flags, seq, (uint64_t)seq * 1000, err_code,
                           payload, plen, out, sizeof(out));
    if (n == 0)
        return -1;
    return (int)sendto(fd, out, n, 0, (const struct sockaddr *)peer, peer_len);
}

static int recv_expect(int fd, uint32_t seq, uint8_t expect_type, unsigned timeout_ms,
                       icpc_header_t *hdr_out)
{
    uint64_t deadline = monotonic_ms() + timeout_ms;

    for (;;) {
        uint64_t now = monotonic_ms();
        if (now >= deadline)
            return -1;

        unsigned remaining_ms = (unsigned)(deadline - now);
        fd_set rfds;
        FD_ZERO(&rfds);
        FD_SET(fd, &rfds);
        struct timeval tv = {
            .tv_sec = (time_t)(remaining_ms / 1000),
            .tv_usec = (suseconds_t)((remaining_ms % 1000) * 1000),
        };
        int ready = select(fd + 1, &rfds, NULL, NULL, &tv);
        if (ready == 0)
            return -1;
        if (ready < 0) {
            if (errno == EINTR)
                continue;
            return -1;
        }

        uint8_t rx[ICPC_MAX_FRAME];
        struct sockaddr_in from;
        socklen_t from_len = sizeof(from);
        ssize_t n = recvfrom(fd, rx, sizeof(rx), 0, (struct sockaddr *)&from, &from_len);
        if (n <= 0)
            continue;

        icpc_header_t hdr;
        const uint8_t *payload = NULL;
        if (icpc_decode(rx, (size_t)n, &hdr, &payload) < 0)
            continue;
        if (hdr.msg_type != expect_type || hdr.seq != seq)
            continue;

        if (expect_type == ICPC_TYPE_STATE_REPORT) {
            if (hdr.payload_len < 8 || memcmp(payload, "state=ok", 8) != 0)
                return -1;
        }

        if (hdr_out)
            *hdr_out = hdr;
        return 0;
    }
}

static int stop_and_wait(uint8_t req_type, uint8_t flags, uint32_t seq, uint16_t err_code,
                         const uint8_t *payload, size_t plen, uint8_t expect_type)
{
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0)
        return -1;

    struct sockaddr_in peer;
    memset(&peer, 0, sizeof(peer));
    peer.sin_family = AF_INET;
    peer.sin_port = htons(ICPC_PORT);
    if (inet_pton(AF_INET, PEER_IP, &peer.sin_addr) != 1) {
        close(fd);
        return -1;
    }

    for (unsigned attempt = 0; attempt <= MAX_RETRIES; attempt++) {
        if (send_frame(fd, &peer, sizeof(peer), req_type, flags, seq, err_code, payload,
                       plen) < 0) {
            close(fd);
            return -1;
        }
        if (recv_expect(fd, seq, expect_type, backoff_ms(attempt), NULL) == 0) {
            close(fd);
            return 0;
        }
        if (attempt < MAX_RETRIES) {
            total_retries++;
            usleep(backoff_ms(attempt) * 1000);
        }
    }

    close(fd);
    return -1;
}

int main(void)
{
    printf("ICPC_RELIABILITY_START\n");

    if (stop_and_wait(ICPC_TYPE_CTRL_CMD, ICPC_FLAG_NEED_ACK, 100, 0,
                      (const uint8_t *)"kp=1.2", 6, ICPC_TYPE_STATE_REPORT) != 0) {
        fprintf(stderr, "reliability: CTRL_CMD failed after retries=%u\n", total_retries);
        return 1;
    }
    printf("ICPC_RELIABILITY_CTRL ok retries=%u\n", total_retries);

    if (stop_and_wait(ICPC_TYPE_ERROR_NOTIFY, ICPC_FLAG_NEED_ACK, 101, 42, NULL, 0,
                      ICPC_TYPE_ACK) != 0) {
        fprintf(stderr, "reliability: ERROR_NOTIFY failed retries=%u\n", total_retries);
        return 1;
    }
    printf("ICPC_RELIABILITY_ERROR ok retries=%u\n", total_retries);

    if (stop_and_wait(ICPC_TYPE_HEARTBEAT, 0, 102, 0, NULL, 0, ICPC_TYPE_HEARTBEAT) != 0) {
        fprintf(stderr, "reliability: HEARTBEAT failed retries=%u\n", total_retries);
        return 1;
    }
    printf("ICPC_RELIABILITY_HEARTBEAT ok retries=%u\n", total_retries);

    printf("icpc-fault-inject pass\n");
    return 0;
}
