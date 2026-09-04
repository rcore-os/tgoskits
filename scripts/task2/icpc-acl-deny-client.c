#define _GNU_SOURCE
#include "icpc-wire.h"

#include <arpa/inet.h>
#include <netinet/in.h>
#include <stdio.h>
#include <string.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

#define PEER_IP "10.0.9.3"
#define DENY_PORT 12345
#define DENY_PROBES 5
#define RECV_TIMEOUT_MS 1500

static uint64_t monotonic_us(void)
{
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0)
        return 0;
    return (uint64_t)ts.tv_sec * 1000000 + (uint64_t)ts.tv_nsec / 1000;
}

static int send_denied_udp(int fd, const struct sockaddr_in *peer, socklen_t peer_len)
{
    static const char payload[] = "ACL_PROBE";
    return (int)sendto(fd, payload, sizeof(payload) - 1, 0,
                       (const struct sockaddr *)peer, peer_len);
}

static int send_icpc_heartbeat(int fd, const struct sockaddr_in *peer, socklen_t peer_len)
{
    uint8_t out[ICPC_MAX_FRAME];
    size_t n = icpc_encode(ICPC_TYPE_HEARTBEAT, 0, 42, 42000, 0, NULL, 0, out,
                           sizeof(out));
    if (n == 0)
        return -1;
    if (sendto(fd, out, n, 0, (const struct sockaddr *)peer, peer_len) < 0)
        return -1;

    uint64_t deadline = monotonic_us() + (uint64_t)RECV_TIMEOUT_MS * 1000;
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
        if (ready <= 0)
            return ready == 0 ? -1 : -1;

        uint8_t rx[ICPC_MAX_FRAME];
        struct sockaddr_in from;
        socklen_t from_len = sizeof(from);
        ssize_t rxn = recvfrom(fd, rx, sizeof(rx), 0, (struct sockaddr *)&from, &from_len);
        if (rxn <= 0)
            continue;

        icpc_header_t hdr;
        if (icpc_decode(rx, (size_t)rxn, &hdr, NULL) < 0)
            continue;
        if (hdr.msg_type == ICPC_TYPE_HEARTBEAT && hdr.seq == 42)
            return 0;
    }
}

int main(void)
{
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0)
        return 1;

    struct sockaddr_in deny_peer;
    memset(&deny_peer, 0, sizeof(deny_peer));
    deny_peer.sin_family = AF_INET;
    deny_peer.sin_port = htons(DENY_PORT);
    if (inet_pton(AF_INET, PEER_IP, &deny_peer.sin_addr) != 1) {
        close(fd);
        return 1;
    }

    struct sockaddr_in icpc_peer = deny_peer;
    icpc_peer.sin_port = htons(ICPC_PORT);

    printf("ICPC_ACL_DENY_BEGIN\n");
    for (int i = 0; i < DENY_PROBES; i++) {
        if (send_denied_udp(fd, &deny_peer, sizeof(deny_peer)) < 0) {
            fprintf(stderr, "icpc-acl-deny: send denied udp failed\n");
            close(fd);
            return 1;
        }
    }
    printf("ICPC_ACL_DENY_SENT probes=%d port=%d\n", DENY_PROBES, DENY_PORT);

    usleep(500000);

    if (send_icpc_heartbeat(fd, &icpc_peer, sizeof(icpc_peer)) != 0) {
        fprintf(stderr, "icpc-acl-deny: icpc heartbeat failed\n");
        close(fd);
        return 1;
    }
    printf("ICPC_ACL_ICPC_OK\n");

    close(fd);
    printf("icpc-acl-deny pass\n");
    return 0;
}
