// SPDX-License-Identifier: GPL-2.0
//
// Linux ping-socket regression.  Toybox and other ping clients use SOCK_DGRAM
// with IPPROTO_ICMP rather than receiving an IPv4 header from a raw socket.

#include "test_framework.h"

#include <arpa/inet.h>
#include <netinet/ip_icmp.h>
#include <poll.h>
#include <stdint.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <unistd.h>

struct echo_packet {
    struct icmphdr header;
    uint32_t timestamp;
};

static uint16_t internet_checksum(const void *data, size_t len) {
    const uint8_t *bytes = data;
    uint32_t sum = 0;

    while (len >= 2) {
        sum += ((uint16_t)bytes[0] << 8) | bytes[1];
        bytes += 2;
        len -= 2;
    }
    if (len != 0) {
        sum += (uint16_t)bytes[0] << 8;
    }
    while ((sum >> 16) != 0) {
        sum = (sum & 0xffffU) + (sum >> 16);
    }
    return htons((uint16_t)~sum);
}

int main(void) {
    TEST_START("IPv4 ICMP ping socket");

    errno = 0;
    int fd = socket(AF_INET, SOCK_DGRAM, IPPROTO_ICMP);
    CHECK(fd >= 0, "socket(AF_INET, SOCK_DGRAM, IPPROTO_ICMP) succeeds");
    if (fd < 0) {
        TEST_DONE();
    }

    int socket_type = 0;
    socklen_t option_len = sizeof(socket_type);
    errno = 0;
    int rc = getsockopt(fd, SOL_SOCKET, SO_TYPE, &socket_type, &option_len);
    CHECK(rc == 0 && socket_type == SOCK_DGRAM,
          "SO_TYPE reports SOCK_DGRAM for the ping socket");

    int receive_ttl = 1;
    errno = 0;
    rc = setsockopt(fd, IPPROTO_IP, IP_RECVTTL, &receive_ttl, sizeof(receive_ttl));
    CHECK(rc == 0, "IP_RECVTTL is accepted for the ping socket");

    struct echo_packet request = {0};
    request.header.type = ICMP_ECHO;
    request.header.un.echo.id = htons((uint16_t)getpid());
    request.header.un.echo.sequence = htons(1);
    request.timestamp = htonl(0x53544152U);
    request.header.checksum = internet_checksum(&request, sizeof(request));

    struct sockaddr_in loopback = {
        .sin_family = AF_INET,
        .sin_addr.s_addr = htonl(INADDR_LOOPBACK),
    };
    errno = 0;
    ssize_t sent = sendto(fd, &request, sizeof(request), 0,
                          (const struct sockaddr *)&loopback, sizeof(loopback));
    CHECK(sent == (ssize_t)sizeof(request), "ICMP echo request is sent to loopback");

    struct pollfd poll_fd = {
        .fd = fd,
        .events = POLLIN,
    };
    errno = 0;
    rc = poll(&poll_fd, 1, 2000);
    CHECK(rc == 1 && (poll_fd.revents & POLLIN) != 0,
          "loopback ICMP echo reply becomes readable");

    if (rc == 1 && (poll_fd.revents & POLLIN) != 0) {
        struct echo_packet reply = {0};
        struct sockaddr_in source = {0};
        struct iovec iov = {
            .iov_base = &reply,
            .iov_len = sizeof(reply),
        };
        char control[CMSG_SPACE(sizeof(int))] = {0};
        struct msghdr message = {
            .msg_name = &source,
            .msg_namelen = sizeof(source),
            .msg_iov = &iov,
            .msg_iovlen = 1,
            .msg_control = control,
            .msg_controllen = sizeof(control),
        };
        errno = 0;
        ssize_t received = recvmsg(fd, &message, 0);
        CHECK(received == (ssize_t)sizeof(reply), "ICMP echo reply payload is received");
        CHECK(reply.header.type == ICMP_ECHOREPLY,
              "received ICMP packet is an echo reply");
        CHECK(reply.header.un.echo.sequence == request.header.un.echo.sequence,
              "echo reply preserves the request sequence");
        CHECK(source.sin_family == AF_INET &&
                  source.sin_addr.s_addr == htonl(INADDR_LOOPBACK),
              "echo reply source is IPv4 loopback");

        int received_ttl = 0;
        int saw_ipv4_ttl = 0;
        /* musl's CMSG_NXTHDR macro mixes signed and unsigned length
         * arithmetic. Keep the warning suppression scoped to the macro. */
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wsign-compare"
        for (struct cmsghdr *cmsg = CMSG_FIRSTHDR(&message); cmsg != NULL;
             cmsg = CMSG_NXTHDR(&message, cmsg)) {
            if (cmsg->cmsg_level == IPPROTO_IP && cmsg->cmsg_type == IP_TTL &&
                cmsg->cmsg_len >= CMSG_LEN(sizeof(received_ttl))) {
                memcpy(&received_ttl, CMSG_DATA(cmsg), sizeof(received_ttl));
                saw_ipv4_ttl = 1;
                break;
            }
        }
#pragma GCC diagnostic pop
        CHECK(saw_ipv4_ttl && received_ttl > 0 && received_ttl <= 255,
              "IP_RECVTTL returns IP_TTL as a 32-bit control message");
    }

    close(fd);
    TEST_DONE();
}
