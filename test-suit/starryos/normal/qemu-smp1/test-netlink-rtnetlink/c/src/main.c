// SPDX-License-Identifier: GPL-2.0
//
// AF_NETLINK / NETLINK_ROUTE smoke test.  Verifies the basic socket
// shape and that RTM_GETLINK returns a multipart dump response instead
// of silently accepting the request and leaving the receive queue empty.
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

struct rtattr_inl {
    unsigned short rta_len;
    unsigned short rta_type;
};

#define NLMSG_REQUEST 0x01
#define NLMSG_DUMP    0x300
#define NLMSG_DONE    3
#define RTM_GETLINK   18
#define RTM_NEWLINK   16
#define IFLA_IFNAME   3

#define NLMSG_ALIGNTO 4U
#define NLMSG_ALIGN(len) (((len) + NLMSG_ALIGNTO - 1) & ~(NLMSG_ALIGNTO - 1))
#define NLMSG_HDRLEN ((int)NLMSG_ALIGN(sizeof(struct nlmsghdr_inl)))
#define NLMSG_LENGTH(len) ((len) + NLMSG_HDRLEN)
#define NLMSG_DATA(nlh) ((void *)((char *)(nlh) + NLMSG_LENGTH(0)))
#define NLMSG_NEXT(nlh, len)                                                                    \
    ((len) -= NLMSG_ALIGN((nlh)->nlmsg_len),                                                    \
     (struct nlmsghdr_inl *)((char *)(nlh) + NLMSG_ALIGN((nlh)->nlmsg_len)))
#define NLMSG_OK(nlh, len)                                                                       \
    ((len) >= (int)sizeof(struct nlmsghdr_inl) &&                                                \
     (nlh)->nlmsg_len >= sizeof(struct nlmsghdr_inl) &&                                          \
     (nlh)->nlmsg_len <= (unsigned int)(len))

#define RTA_ALIGNTO 4U
#define RTA_ALIGN(len) (((len) + RTA_ALIGNTO - 1) & ~(RTA_ALIGNTO - 1))
#define RTA_LENGTH(len) (RTA_ALIGN(sizeof(struct rtattr_inl)) + (len))
#define RTA_DATA(rta) ((void *)((char *)(rta) + RTA_LENGTH(0)))
#define RTA_NEXT(rta, attrlen)                                                                   \
    ((attrlen) -= RTA_ALIGN((rta)->rta_len),                                                     \
     (struct rtattr_inl *)((char *)(rta) + RTA_ALIGN((rta)->rta_len)))
#define RTA_OK(rta, len)                                                                         \
    ((len) >= (int)sizeof(struct rtattr_inl) && (rta)->rta_len >= sizeof(struct rtattr_inl) &&    \
     (rta)->rta_len <= (unsigned int)(len))
#define IFLA_RTA(r) ((struct rtattr_inl *)(((char *)(r)) + NLMSG_ALIGN(sizeof(struct ifinfomsg_inl))))

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

    // Send a well-formed RTM_GETLINK dump request. The kernel must return
    // at least one RTM_NEWLINK and terminate the multipart dump.
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

    int fl = fcntl(fd, F_GETFL, 0);
    CHECK_RET(fcntl(fd, F_SETFL, fl | O_NONBLOCK), 0, "F_SETFL O_NONBLOCK");

    char buf[4096];
    int saw_link = 0;
    int saw_lo = 0;
    int saw_done = 0;
    errno = 0;
    ssize_t n = read(fd, buf, sizeof(buf));
    CHECK(n > 0, "read(RTM_GETLINK response) returns data");
    if (n > 0) {
        int remaining = (int)n;
        for (struct nlmsghdr_inl *nh = (struct nlmsghdr_inl *)buf; NLMSG_OK(nh, remaining);
             nh = NLMSG_NEXT(nh, remaining)) {
            if (nh->nlmsg_type == NLMSG_DONE) {
                saw_done = 1;
                break;
            }
            if (nh->nlmsg_type != RTM_NEWLINK) {
                continue;
            }

            saw_link = 1;
            struct ifinfomsg_inl *ifi = NLMSG_DATA(nh);
            int len = (int)nh->nlmsg_len - NLMSG_LENGTH(sizeof(*ifi));
            for (struct rtattr_inl *attr = IFLA_RTA(ifi); RTA_OK(attr, len);
                 attr = RTA_NEXT(attr, len)) {
                if (attr->rta_type == IFLA_IFNAME &&
                    strcmp((const char *)RTA_DATA(attr), "lo") == 0) {
                    saw_lo = 1;
                }
            }
        }
    }
    CHECK(saw_link, "RTM_GETLINK returns at least one RTM_NEWLINK");
    CHECK(saw_lo, "RTM_GETLINK includes loopback ifname");
    CHECK(saw_done, "RTM_GETLINK dump ends with NLMSG_DONE");

    // Regression: sending another netlink request must not clear
    // responses still sitting in the receive queue. Earlier
    // revisions clear()ed the queue on every write, which would
    // drop async broadcasts (uevent, rtnetlink events) that arrived
    // before a `send(RTM_GETLINK)`. We can't deterministically
    // trigger a broadcast from user-space, but stacking two
    // RTM_GETLINK requests back-to-back without reading exercises
    // the same code path: the second request enqueues a second
    // response and the first response must survive.
    int qfd = socket(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE);
    CHECK(qfd >= 0, "socket(AF_NETLINK, NETLINK_ROUTE) for queue test");
    if (qfd >= 0) {
        req.nh.nlmsg_seq = 0xabcd;
        CHECK(send(qfd, &req, sizeof(req), 0) == (ssize_t)sizeof(req),
              "first RTM_GETLINK send (queue test)");
        req.nh.nlmsg_seq = 0xabce;
        CHECK(send(qfd, &req, sizeof(req), 0) == (ssize_t)sizeof(req),
              "second RTM_GETLINK send (queue test)");

        unsigned char rb[4096];
        ssize_t r_first = read(qfd, rb, sizeof(rb));
        CHECK(r_first > 0, "first response survives second send");
        if (r_first > 0) {
            struct nlmsghdr_inl *h = (struct nlmsghdr_inl *)rb;
            CHECK(h->nlmsg_seq == 0xabcd,
                  "first read returns the first send's seq, not a clobbered queue");
        }
        ssize_t r_second = read(qfd, rb, sizeof(rb));
        CHECK(r_second > 0, "second response also queued");

        close(qfd);
    }

    close(fd);
    TEST_DONE();
}
