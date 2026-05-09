// SPDX-License-Identifier: GPL-2.0
//
// AF_NETLINK / NETLINK_GENERIC smoke test.  Exercises the genl socket
// surface that genl-ctrl-list / libnl-genl drive: socket() with protocol
// 16, bind(), and a CTRL_CMD_GETFAMILY dump request that the kernel
// drains.  No genl family registry exists yet — the test asserts the
// transport surface is wired (no crash, no EPROTONOSUPPORT, accepts
// well-formed dump request).

#include "test_framework.h"

#include <fcntl.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <unistd.h>

#ifndef AF_NETLINK
#define AF_NETLINK 16
#endif
#ifndef NETLINK_GENERIC
#define NETLINK_GENERIC 16
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

struct genlmsghdr_inl {
    unsigned char cmd;
    unsigned char version;
    unsigned short reserved;
};

#define NLMSG_REQUEST 0x01
#define NLMSG_DUMP    0x300
#define GENL_ID_CTRL  0x10
#define CTRL_CMD_GETFAMILY 3

int main(void) {
    TEST_START("netlink genl");

    int fd = socket(AF_NETLINK, SOCK_RAW, NETLINK_GENERIC);
    CHECK(fd >= 0, "socket(AF_NETLINK, SOCK_RAW, NETLINK_GENERIC)");
    if (fd < 0) {
        TEST_DONE();
    }

    struct sockaddr_nl_inl addr = {0};
    addr.nl_family = AF_NETLINK;
    addr.nl_pid = 0;
    addr.nl_groups = 0;
    CHECK_RET(bind(fd, (struct sockaddr *)&addr, sizeof(addr)), 0,
              "bind genl socket");

    // CTRL_CMD_GETFAMILY dump — what `genl-ctrl-list` sends first.
    struct {
        struct nlmsghdr_inl   nh;
        struct genlmsghdr_inl gh;
    } req = {0};
    req.nh.nlmsg_len = sizeof(req);
    req.nh.nlmsg_type = GENL_ID_CTRL;
    req.nh.nlmsg_flags = NLMSG_REQUEST | NLMSG_DUMP;
    req.nh.nlmsg_seq = 1;
    req.nh.nlmsg_pid = 0;
    req.gh.cmd = CTRL_CMD_GETFAMILY;
    req.gh.version = 1;

    ssize_t sent = send(fd, &req, sizeof(req), 0);
    CHECK(sent == (ssize_t)sizeof(req),
          "send(CTRL_CMD_GETFAMILY) drained");

    // Non-blocking probe: empty queue must report EAGAIN cleanly.
    int fl = fcntl(fd, F_GETFL, 0);
    CHECK_RET(fcntl(fd, F_SETFL, fl | O_NONBLOCK), 0, "F_SETFL O_NONBLOCK");

    char buf[4096];
    errno = 0;
    ssize_t n = read(fd, buf, sizeof(buf));
    if (n >= 0) {
        printf("  PASS | genl read returned %ld bytes\n", n);
        __pass++;
    } else {
        CHECK(errno == EAGAIN || errno == EWOULDBLOCK,
              "empty genl queue → EAGAIN");
    }

    // A SOCK_DGRAM netlink socket should also work (libnl uses both).
    int fd2 = socket(AF_NETLINK, SOCK_DGRAM, NETLINK_GENERIC);
    CHECK(fd2 >= 0, "socket SOCK_DGRAM netlink");
    if (fd2 >= 0) close(fd2);

    close(fd);
    TEST_DONE();
}
