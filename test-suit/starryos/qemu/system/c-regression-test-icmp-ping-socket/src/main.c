// SPDX-License-Identifier: GPL-2.0
//
// Linux ping-socket regression.  Toybox and other unprivileged ping clients
// use SOCK_DGRAM with IPPROTO_ICMP rather than a privileged raw socket.

#include "test_framework.h"

#include <arpa/inet.h>
#include <netinet/ip_icmp.h>
#include <poll.h>
#include <stdint.h>
#include <sys/socket.h>
#include <sys/types.h>
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
        socklen_t source_len = sizeof(source);
        errno = 0;
        ssize_t received = recvfrom(fd, &reply, sizeof(reply), 0,
                                    (struct sockaddr *)&source, &source_len);
        CHECK(received == (ssize_t)sizeof(reply), "ICMP echo reply payload is received");
        CHECK(reply.header.type == ICMP_ECHOREPLY,
              "received ICMP packet is an echo reply");
        CHECK(reply.header.un.echo.sequence == request.header.un.echo.sequence,
              "echo reply preserves the request sequence");
        CHECK(source.sin_family == AF_INET &&
                  source.sin_addr.s_addr == htonl(INADDR_LOOPBACK),
              "echo reply source is IPv4 loopback");
    }

    close(fd);
    TEST_DONE();
}
