#define _GNU_SOURCE
#include "icpc-wire.h"

#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

#define PEER_IP "10.0.9.3"
#define DEFAULT_MSGS 20
#define RECV_TIMEOUT_MS 2000

static uint64_t monotonic_us(void)
{
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0)
        return 0;
    return (uint64_t)ts.tv_sec * 1000000 + (uint64_t)ts.tv_nsec / 1000;
}

static int cmp_u64(const void *a, const void *b)
{
    uint64_t lhs = *(const uint64_t *)a;
    uint64_t rhs = *(const uint64_t *)b;
    if (lhs < rhs)
        return -1;
    if (lhs > rhs)
        return 1;
    return 0;
}

static uint64_t percentile_us(uint64_t *samples, size_t n, unsigned pct)
{
    if (n == 0)
        return 0;
    qsort(samples, n, sizeof(*samples), cmp_u64);
    size_t idx = (n * pct) / 100;
    if (idx >= n)
        idx = n - 1;
    return samples[idx];
}

static int send_frame(int fd, const struct sockaddr_in *peer, socklen_t peer_len,
                      uint8_t msg_type, uint32_t seq)
{
    uint8_t out[ICPC_MAX_FRAME];
    size_t n = icpc_encode(msg_type, 0, seq, (uint64_t)seq * 1000, 0, NULL, 0, out,
                           sizeof(out));
    if (n == 0)
        return -1;
    return (int)sendto(fd, out, n, 0, (const struct sockaddr *)peer, peer_len);
}

static int recv_heartbeat(int fd, uint32_t seq, unsigned timeout_ms)
{
    uint64_t deadline = monotonic_us() + (uint64_t)timeout_ms * 1000;

    for (;;) {
        uint64_t now = monotonic_us();
        if (now >= deadline)
            return -1;

        unsigned remaining_ms = (unsigned)((deadline - now) / 1000);
        if (remaining_ms == 0)
            remaining_ms = 1;

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
        if (icpc_decode(rx, (size_t)n, &hdr, NULL) < 0)
            continue;
        if (hdr.msg_type != ICPC_TYPE_HEARTBEAT || hdr.seq != seq)
            continue;
        return 0;
    }
}

int main(int argc, char **argv)
{
    unsigned msg_count = DEFAULT_MSGS;
    if (argc > 1)
        msg_count = (unsigned)strtoul(argv[1], NULL, 10);
    if (msg_count == 0 || msg_count > 1000)
        msg_count = DEFAULT_MSGS;

    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0)
        return 1;

    struct sockaddr_in peer;
    memset(&peer, 0, sizeof(peer));
    peer.sin_family = AF_INET;
    peer.sin_port = htons(ICPC_PORT);
    if (inet_pton(AF_INET, PEER_IP, &peer.sin_addr) != 1) {
        close(fd);
        return 1;
    }

    uint64_t *rtt_us = calloc(msg_count, sizeof(*rtt_us));
    if (!rtt_us) {
        close(fd);
        return 1;
    }

    unsigned ok = 0;
    unsigned fail = 0;
    uint64_t bench_start = monotonic_us();

    printf("ICPC_BENCH_CSV\n");
    printf("seq,rtt_us,ok\n");

    for (unsigned i = 0; i < msg_count; i++) {
        uint32_t seq = 1000 + i;
        uint64_t t0 = monotonic_us();
        int sent = send_frame(fd, &peer, sizeof(peer), ICPC_TYPE_HEARTBEAT, seq);
        int got = (sent >= 0) ? recv_heartbeat(fd, seq, RECV_TIMEOUT_MS) : -1;
        uint64_t dt = monotonic_us() - t0;

        if (got == 0) {
            rtt_us[ok] = dt;
            ok++;
            printf("%u,%llu,1\n", seq, (unsigned long long)dt);
        } else {
            fail++;
            printf("%u,%llu,0\n", seq, (unsigned long long)dt);
        }
    }

    close(fd);

    uint64_t elapsed_us = monotonic_us() - bench_start;
    if (elapsed_us == 0)
        elapsed_us = 1;

    double msg_per_s = (double)ok * 1000000.0 / (double)elapsed_us;
    uint64_t p50 = percentile_us(rtt_us, ok, 50);
    uint64_t p99 = percentile_us(rtt_us, ok, 99);

    printf("ICPC_BENCH_SUMMARY msgs=%u ok=%u fail=%u p50_us=%llu p99_us=%llu msg_per_s=%.2f\n",
           msg_count, ok, fail, (unsigned long long)p50, (unsigned long long)p99, msg_per_s);

    free(rtt_us);

    if (fail > 0 || ok == 0) {
        fprintf(stderr, "icpc-bench: failures=%u\n", fail);
        return 1;
    }

    printf("icpc-bench pass\n");
    return 0;
}
