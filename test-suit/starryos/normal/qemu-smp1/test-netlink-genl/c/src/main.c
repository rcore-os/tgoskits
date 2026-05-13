// SPDX-License-Identifier: GPL-2.0
//
// AF_NETLINK / NETLINK_GENERIC smoke test. Exercises the genl socket
// surface that genl-ctrl-list / libnl-genl drive: socket() with
// protocol 16, bind(), CTRL_CMD_GETFAMILY dump request, and read-back
// of the controller's CTRL_CMD_NEWFAMILY response with NLMSG_DONE
// terminator.

#include "test_framework.h"

#include <fcntl.h>
#include <string.h>
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
#define NLMSG_DONE    0x3
#define GENL_ID_CTRL  0x10
#define CTRL_CMD_NEWFAMILY 1
#define CTRL_CMD_GETFAMILY 3
#define CTRL_ATTR_FAMILY_ID   1
#define CTRL_ATTR_FAMILY_NAME 2

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

    /* Read back the controller's response. With a dump request, the
     * kernel must reply with at least one CTRL_CMD_NEWFAMILY message
     * carrying the controller family's own ID + name, followed by
     * NLMSG_DONE. */
    char buf[4096];
    ssize_t n = recv(fd, buf, sizeof(buf), 0);
    CHECK(n > 0, "recv returned a non-empty response");

    /* Parse: first nlmsghdr should be GENL_ID_CTRL. */
    if (n >= (ssize_t)sizeof(struct nlmsghdr_inl)) {
        struct nlmsghdr_inl *nh = (struct nlmsghdr_inl *)buf;
        CHECK(nh->nlmsg_type == GENL_ID_CTRL,
              "first reply nlmsg_type == GENL_ID_CTRL");

        /* genlmsghdr follows the nlmsghdr. */
        if (nh->nlmsg_len >= sizeof(struct nlmsghdr_inl) + sizeof(struct genlmsghdr_inl)) {
            struct genlmsghdr_inl *gh =
                (struct genlmsghdr_inl *)(buf + sizeof(struct nlmsghdr_inl));
            CHECK(gh->cmd == CTRL_CMD_NEWFAMILY,
                  "genlmsghdr.cmd == CTRL_CMD_NEWFAMILY");
        }

        /* Walk forward to find NLMSG_DONE. */
        unsigned off = (nh->nlmsg_len + 3u) & ~3u;
        int saw_done = 0;
        while (off + sizeof(struct nlmsghdr_inl) <= (unsigned)n) {
            struct nlmsghdr_inl *next = (struct nlmsghdr_inl *)(buf + off);
            if (next->nlmsg_type == NLMSG_DONE) { saw_done = 1; break; }
            if (next->nlmsg_len < sizeof(struct nlmsghdr_inl)) break;
            off += (next->nlmsg_len + 3u) & ~3u;
        }
        CHECK(saw_done, "response terminated by NLMSG_DONE");
    }

    /* Asking for a non-existent family must return NLMSG_ERROR / -ENOENT,
     * not a blank dump. */
    struct {
        struct nlmsghdr_inl   nh;
        struct genlmsghdr_inl gh;
        struct { unsigned short len; unsigned short type; } a;
        char name[16];
    } req_name = {0};
    req_name.nh.nlmsg_len = sizeof(req_name);
    req_name.nh.nlmsg_type = GENL_ID_CTRL;
    req_name.nh.nlmsg_flags = NLMSG_REQUEST;
    req_name.nh.nlmsg_seq = 2;
    req_name.gh.cmd = CTRL_CMD_GETFAMILY;
    req_name.gh.version = 1;
    req_name.a.type = CTRL_ATTR_FAMILY_NAME;
    const char *unknown = "nonexistent-family";
    size_t namelen = 0;
    while (unknown[namelen] && namelen < sizeof(req_name.name) - 1) {
        req_name.name[namelen] = unknown[namelen]; namelen++;
    }
    req_name.name[namelen++] = 0;
    req_name.a.len = sizeof(req_name.a) + namelen;

    sent = send(fd, &req_name, sizeof(req_name), 0);
    CHECK(sent == (ssize_t)sizeof(req_name), "send(GETFAMILY unknown)");

    n = recv(fd, buf, sizeof(buf), 0);
    CHECK(n >= (ssize_t)(sizeof(struct nlmsghdr_inl) + sizeof(int)),
          "recv returned an nlmsgerr payload");
    if (n >= (ssize_t)(sizeof(struct nlmsghdr_inl) + sizeof(int))) {
        struct nlmsghdr_inl *nh = (struct nlmsghdr_inl *)buf;
        int err;
        memcpy(&err, buf + sizeof(struct nlmsghdr_inl), sizeof(err));
        CHECK(nh->nlmsg_type == 2 /* NLMSG_ERROR */ && err == -2 /* -ENOENT */,
              "unknown family → NLMSG_ERROR with -ENOENT");
    }

    /* A SOCK_DGRAM netlink socket should also work (libnl uses both). */
    int fd2 = socket(AF_NETLINK, SOCK_DGRAM, NETLINK_GENERIC);
    CHECK(fd2 >= 0, "socket SOCK_DGRAM netlink");
    if (fd2 >= 0) close(fd2);

    close(fd);
    TEST_DONE();
}
