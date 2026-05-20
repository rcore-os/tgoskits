// SPDX-License-Identifier: GPL-2.0
//
// AF_NETLINK / NETLINK_KOBJECT_UEVENT smoke test.  This is the libudev
// monitor path: socket() with SOCK_RAW + protocol 15, bind() with a
// non-zero nl_groups bitmask to subscribe, setsockopt SO_RCVBUF to size
// the buffer, and a non-blocking read that should return EAGAIN when no
// uevents have been broadcast.  Real broadcast emission is exercised in
// downstream M5 work (kernel device hotplug); this test pins the socket
// surface that libudev relies on.

#include "test_framework.h"

#include <fcntl.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <unistd.h>

#ifndef AF_NETLINK
#define AF_NETLINK 16
#endif
#ifndef NETLINK_KOBJECT_UEVENT
#define NETLINK_KOBJECT_UEVENT 15
#endif
#ifndef SO_RCVBUF
#define SO_RCVBUF 8
#endif
#ifndef SOL_SOCKET
#define SOL_SOCKET 1
#endif

struct sockaddr_nl_inl {
    unsigned short nl_family;
    unsigned short nl_pad;
    unsigned int   nl_pid;
    unsigned int   nl_groups;
};

int main(void) {
    TEST_START("netlink uevent");

    int fd = socket(AF_NETLINK, SOCK_RAW, NETLINK_KOBJECT_UEVENT);
    CHECK(fd >= 0,
          "socket(AF_NETLINK, SOCK_RAW, NETLINK_KOBJECT_UEVENT)");
    if (fd < 0) {
        TEST_DONE();
    }

    int rcvbuf = 256 * 1024;
    CHECK_RET(setsockopt(fd, SOL_SOCKET, SO_RCVBUF,
                         &rcvbuf, sizeof(rcvbuf)),
              0, "setsockopt(SO_RCVBUF)");

    struct sockaddr_nl_inl addr = {0};
    addr.nl_family = AF_NETLINK;
    addr.nl_pid = 0;
    addr.nl_groups = 1;  // group 1 = "kernel" uevent broadcasts
    CHECK_RET(bind(fd, (struct sockaddr *)&addr, sizeof(addr)), 0,
              "bind(nl_groups=1)");

    struct sockaddr_nl_inl got = {0};
    socklen_t slen = sizeof(got);
    CHECK_RET(getsockname(fd, (struct sockaddr *)&got, &slen), 0,
              "getsockname");
    CHECK(got.nl_groups == 1, "getsockname preserves nl_groups");

    // No emitter has fired in the test boot — non-blocking read must
    // report EAGAIN, not block, not crash.
    int fl = fcntl(fd, F_GETFL, 0);
    CHECK_RET(fcntl(fd, F_SETFL, fl | O_NONBLOCK), 0, "F_SETFL O_NONBLOCK");

    char buf[8192];
    errno = 0;
    ssize_t n = read(fd, buf, sizeof(buf));
    CHECK(n == -1 && (errno == EAGAIN || errno == EWOULDBLOCK),
          "empty uevent queue → EAGAIN");

    // A second independent socket should bind cleanly (registry must
    // accept multiple subscribers).
    int fd2 = socket(AF_NETLINK, SOCK_RAW, NETLINK_KOBJECT_UEVENT);
    CHECK(fd2 >= 0, "second uevent socket");
    if (fd2 >= 0) {
        struct sockaddr_nl_inl a2 = {0};
        a2.nl_family = AF_NETLINK;
        a2.nl_groups = 2;
        CHECK_RET(bind(fd2, (struct sockaddr *)&a2, sizeof(a2)), 0,
                  "second bind(nl_groups=2)");
        close(fd2);
    }

    close(fd);
    TEST_DONE();
}
