// SPDX-License-Identifier: GPL-2.0
//
// AF_NETLINK / NETLINK_ROUTE smoke test.  Verifies the basic socket
// shape: socket() + bind() + getsockname() round trip, that a sendto()
// of an RTM_GETLINK request is accepted by the kernel side, and that
// reading from an empty queue with O_NONBLOCK returns EAGAIN.  Real
// RTM_GETLINK responder behaviour is intentionally not asserted here
// (the J PR provides the byte transport; the responder lands later).
//
// We inline the netlink uapi structs because musl does not ship
// <linux/netlink.h> / <linux/rtnetlink.h>.

#include "test_framework.h"

#include <fcntl.h>
#include <poll.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <unistd.h>

#ifndef AF_NETLINK
#define AF_NETLINK 16
#endif
#ifndef NETLINK_ROUTE
#define NETLINK_ROUTE 0
#endif

struct sockaddr_nl_inl {
    unsigned short nl_family;
    unsigned short nl_pad;
    unsigned int   nl_pid;
    unsigned int   nl_groups;
};

struct nlmsghdr_inl {
    unsigned int nlmsg_len;
    unsigned short nlmsg_type;
    unsigned short nlmsg_flags;
    unsigned int nlmsg_seq;
    unsigned int nlmsg_pid;
};

struct ifinfomsg_inl {
    unsigned char ifi_family;
    unsigned char __ifi_pad;
    unsigned short ifi_type;
    int ifi_index;
    unsigned int ifi_flags;
    unsigned int ifi_change;
};

#define NLMSG_REQUEST 0x01
#define NLMSG_DUMP    0x300
#define RTM_GETLINK   18

int main(void) {
    TEST_START("netlink rtnetlink");

    int fd = socket(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE);
    CHECK(fd >= 0, "socket(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE)");
    if (fd < 0) {
        TEST_DONE();
    }

    struct sockaddr_nl_inl addr = {0};
    addr.nl_family = AF_NETLINK;
    addr.nl_pid = 0;     // let kernel pick (== getpid())
    addr.nl_groups = 0;  // unicast only
    CHECK_RET(bind(fd, (struct sockaddr *)&addr, sizeof(addr)), 0,
              "bind(sockaddr_nl)");

    struct sockaddr_nl_inl got = {0};
    socklen_t slen = sizeof(got);
    CHECK_RET(getsockname(fd, (struct sockaddr *)&got, &slen), 0,
              "getsockname round-trip");
    CHECK(got.nl_family == AF_NETLINK, "getsockname: nl_family == AF_NETLINK");

    // Send a well-formed RTM_GETLINK dump request.  Kernel currently
    // accepts the bytes (drain-and-ack); responder may or may not exist.
    struct {
        struct nlmsghdr_inl  nh;
        struct ifinfomsg_inl ifi;
    } req = {0};
    req.nh.nlmsg_len = sizeof(req);
    req.nh.nlmsg_type = RTM_GETLINK;
    req.nh.nlmsg_flags = NLMSG_REQUEST | NLMSG_DUMP;
    req.nh.nlmsg_seq = 1;
    req.nh.nlmsg_pid = 0;
    req.ifi.ifi_family = 0; // AF_UNSPEC

    ssize_t sent = send(fd, &req, sizeof(req), 0);
    CHECK(sent == (ssize_t)sizeof(req), "send(RTM_GETLINK) drained by kernel");

    // Drain in non-blocking mode; an empty queue must report EAGAIN, not
    // crash and not block forever.
    int fl = fcntl(fd, F_GETFL, 0);
    CHECK_RET(fcntl(fd, F_SETFL, fl | O_NONBLOCK), 0, "F_SETFL O_NONBLOCK");

    char buf[4096];
    errno = 0;
    ssize_t n = read(fd, buf, sizeof(buf));
    if (n >= 0) {
        // A future responder could legitimately succeed here.  Either way
        // counts as PASS for the transport layer.
        printf("  PASS | empty/responded read returned %ld bytes\n", n);
        __pass++;
    } else {
        CHECK(errno == EAGAIN || errno == EWOULDBLOCK,
              "empty queue → EAGAIN/EWOULDBLOCK");
    }

    close(fd);
    TEST_DONE();
}
