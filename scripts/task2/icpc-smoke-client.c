#define _GNU_SOURCE
#include "icpc-wire.h"

#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <stdio.h>
#include <string.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <unistd.h>

#define PEER_IP "10.0.9.3"
#define TIMEOUT_SEC 15

static int send_and_recv(uint8_t req_type, uint32_t seq, uint16_t err_code,
                         const uint8_t *req_payload, size_t req_plen,
                         uint8_t expect_type, char *detail, size_t detail_cap)
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

    uint8_t tx[ICPC_MAX_FRAME];
    size_t tx_len =
        icpc_encode(req_type, 0, seq, 1000 * (uint64_t)seq, err_code, req_payload,
                    req_plen, tx, sizeof(tx));
    if (tx_len == 0) {
        close(fd);
        return -1;
    }
    if (sendto(fd, tx, tx_len, 0, (struct sockaddr *)&peer, sizeof(peer)) < 0) {
        close(fd);
        return -1;
    }

    for (;;) {
        fd_set rfds;
        FD_ZERO(&rfds);
        FD_SET(fd, &rfds);
        struct timeval tv = {.tv_sec = TIMEOUT_SEC, .tv_usec = 0};
        int ready = select(fd + 1, &rfds, NULL, NULL, &tv);
        if (ready == 0) {
            if (detail_cap > 0)
                snprintf(detail, detail_cap, "timeout waiting for type=%#x", expect_type);
            close(fd);
            return -1;
        }
        if (ready < 0) {
            if (errno == EINTR)
                continue;
            close(fd);
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
        if (hdr.msg_type != expect_type)
            continue;
        if (hdr.seq != seq) {
            if (detail_cap > 0)
                snprintf(detail, detail_cap, "seq mismatch got=%u want=%u", hdr.seq, seq);
            close(fd);
            return -1;
        }

        if (expect_type == ICPC_TYPE_STATE_REPORT) {
            if (hdr.payload_len < 8 ||
                memcmp(payload, "state=ok", 8) != 0) {
                if (detail_cap > 0)
                    snprintf(detail, detail_cap, "bad STATE_REPORT payload");
                close(fd);
                return -1;
            }
        }

        close(fd);
        return 0;
    }
}

int main(void)
{
    char err[128];

    if (send_and_recv(ICPC_TYPE_CTRL_CMD, 1, 0, (const uint8_t *)"kp=1.2", 6,
                      ICPC_TYPE_STATE_REPORT, err, sizeof(err)) != 0) {
        fprintf(stderr, "CTRL_CMD: %s\n", err);
        return 1;
    }
    printf("ICPC_CTRL_OK\n");

    if (send_and_recv(ICPC_TYPE_ERROR_NOTIFY, 2, 42, NULL, 0, ICPC_TYPE_ACK, err,
                      sizeof(err)) != 0) {
        fprintf(stderr, "ERROR_NOTIFY: %s\n", err);
        return 1;
    }
    printf("ICPC_ERROR_OK\n");

    if (send_and_recv(ICPC_TYPE_HEARTBEAT, 3, 0, NULL, 0, ICPC_TYPE_HEARTBEAT, err,
                      sizeof(err)) != 0) {
        fprintf(stderr, "HEARTBEAT: %s\n", err);
        return 1;
    }
    printf("ICPC_HEARTBEAT_OK\n");

    printf("icpc-smoke pass\n");
    return 0;
}
