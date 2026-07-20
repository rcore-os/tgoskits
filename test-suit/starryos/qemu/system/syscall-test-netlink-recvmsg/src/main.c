/*
 * test_netlink_recvmsg.c — recvmsg(2) MSG_PEEK / MSG_TRUNC / MSG_DONTWAIT on an
 * AF_NETLINK (NETLINK_ROUTE) socket.
 *
 * Regression for the bug where NetlinkSocket (a FileLike, not an axnet Socket)
 * ignored the recvmsg flags and always popped the front datagram:
 *   - MSG_PEEK consumed the message instead of leaving it queued,
 *   - MSG_TRUNC reported the copied length instead of the real datagram length,
 *   - MSG_DONTWAIT blocked instead of returning EAGAIN on an empty queue.
 * glibc/musl getifaddrs() does exactly "peek to size the buffer, then read for
 * real", so a consuming peek made the follow-up read block forever.
 *
 * The netlink uAPI (linux/netlink.h, linux/rtnetlink.h) is NOT in the musl
 * cross sysroot, so the few structs/constants we need are defined inline with
 * their fixed kernel-ABI values; this keeps the test self-contained.
 */

#include "test_framework.h"
#include <stdint.h>
#include <unistd.h>
#include <sys/socket.h>

#ifndef NETLINK_ROUTE
#define NETLINK_ROUTE 0
#endif
#define RTM_GETADDR 22
#define NLM_F_REQUEST 0x01
#define NLM_F_ROOT 0x100
#define NLM_F_MATCH 0x200
#define NLM_F_DUMP (NLM_F_ROOT | NLM_F_MATCH)

struct nlmsghdr {
    uint32_t nlmsg_len;
    uint16_t nlmsg_type;
    uint16_t nlmsg_flags;
    uint32_t nlmsg_seq;
    uint32_t nlmsg_pid;
};

struct ifaddrmsg {
    uint8_t ifa_family;
    uint8_t ifa_prefixlen;
    uint8_t ifa_flags;
    uint8_t ifa_scope;
    uint32_t ifa_index;
};

struct sockaddr_nl {
    sa_family_t nl_family;
    unsigned short nl_pad;
    uint32_t nl_pid;
    uint32_t nl_groups;
};

#define NLMSG_ALIGNTO 4u
#define NLMSG_ALIGN(len) (((len) + NLMSG_ALIGNTO - 1) & ~(NLMSG_ALIGNTO - 1))
#define NLMSG_HDRLEN ((int)NLMSG_ALIGN(sizeof(struct nlmsghdr)))
#define NLMSG_LENGTH(len) ((int)(NLMSG_HDRLEN + (len)))

int main(void)
{
    TEST_START("netlink recvmsg MSG_PEEK/MSG_TRUNC/MSG_DONTWAIT");

    int fd = socket(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE);
    CHECK(fd >= 0, "socket(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE)");
    if (fd < 0) {
        TEST_DONE();
    }

    /* ---- MSG_DONTWAIT on an empty queue → EAGAIN (no message sent yet) ---- */
    {
        char buf[64];
        struct iovec iov = {.iov_base = buf, .iov_len = sizeof(buf)};
        struct msghdr mh = {0};
        mh.msg_iov = &iov;
        mh.msg_iovlen = 1;
        CHECK_ERR(recvmsg(fd, &mh, MSG_DONTWAIT), EAGAIN,
                  "recvmsg(MSG_DONTWAIT) on empty queue → EAGAIN");
    }

    /* ---- send an RTM_GETADDR dump request ---- */
    struct {
        struct nlmsghdr nlh;
        struct ifaddrmsg ifa;
    } req;
    memset(&req, 0, sizeof(req));
    req.nlh.nlmsg_len = (uint32_t)NLMSG_LENGTH(sizeof(struct ifaddrmsg));
    req.nlh.nlmsg_type = RTM_GETADDR;
    req.nlh.nlmsg_flags = NLM_F_REQUEST | NLM_F_DUMP;
    req.nlh.nlmsg_seq = 1;
    req.ifa.ifa_family = AF_UNSPEC;

    struct sockaddr_nl kernel = {0};
    kernel.nl_family = AF_NETLINK;
    ssize_t sent = sendto(fd, &req, req.nlh.nlmsg_len, 0,
                          (struct sockaddr *)&kernel, sizeof(kernel));
    CHECK(sent == (ssize_t)req.nlh.nlmsg_len, "sendto RTM_GETADDR dump request");

    /* ---- MSG_PEEK | MSG_TRUNC with a deliberately tiny buffer ----
     * MSG_TRUNC must return the FULL datagram length (the real reply is a full
     * nlmsghdr+payload, well over 16 bytes), proving truncation reporting; and
     * MSG_PEEK must NOT consume it, so the follow-up real read still sees it. */
    char tiny[16];
    struct iovec piov = {.iov_base = tiny, .iov_len = sizeof(tiny)};
    struct msghdr pmh = {0};
    pmh.msg_iov = &piov;
    pmh.msg_iovlen = 1;
    ssize_t peeked = recvmsg(fd, &pmh, MSG_PEEK | MSG_TRUNC);
    CHECK(peeked > (ssize_t)sizeof(tiny),
          "recvmsg(MSG_PEEK|MSG_TRUNC) returns full datagram length (> tiny buf)");
    CHECK((pmh.msg_flags & MSG_TRUNC) != 0,
          "peeked msg_flags has MSG_TRUNC set (buffer was too small)");

    /* ---- real read must still see the peeked datagram (peek didn't consume) ---- */
    char full[8192];
    struct iovec fiov = {.iov_base = full, .iov_len = sizeof(full)};
    struct msghdr fmh = {0};
    fmh.msg_iov = &fiov;
    fmh.msg_iovlen = 1;
    ssize_t got = recvmsg(fd, &fmh, 0);
    CHECK(got > 0, "follow-up recvmsg() still returns the datagram (peek non-destructive)");
    CHECK(peeked == got,
          "peeked length == subsequently read length (same datagram)");

    close(fd);
    TEST_DONE();
}
