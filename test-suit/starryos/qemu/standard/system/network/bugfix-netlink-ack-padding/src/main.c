#define _GNU_SOURCE

#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/uio.h>
#include <unistd.h>

#ifndef AF_NETLINK
#define AF_NETLINK 16
#endif

#define NETLINK_ROUTE 0
#define NLM_F_REQUEST 0x01
#define NLM_F_DUMP 0x300
#define NLMSG_ERROR 2
#define NLMSG_DONE 3
#define RTM_GETLINK 18
#define RTM_GETROUTE 26

#define NLMSG_ALIGNTO 4U
#define NLMSG_ALIGN(len) (((len) + NLMSG_ALIGNTO - 1) & ~(NLMSG_ALIGNTO - 1))
#define NLMSG_HDRLEN ((unsigned int)NLMSG_ALIGN(sizeof(struct nlmsghdr_local)))
#define NLMSG_LENGTH(len) ((unsigned int)(len) + NLMSG_HDRLEN)

struct sockaddr_nl_local {
    unsigned short nl_family;
    unsigned short nl_pad;
    unsigned int nl_pid;
    unsigned int nl_groups;
};

struct nlmsghdr_local {
    unsigned int nlmsg_len;
    unsigned short nlmsg_type;
    unsigned short nlmsg_flags;
    unsigned int nlmsg_seq;
    unsigned int nlmsg_pid;
};

/* The canonical rtnetlink dump request payload is a single byte, so a request
 * built with NLMSG_LENGTH(sizeof(struct rtgenmsg)) is 17 bytes long and is not
 * a multiple of NLMSG_ALIGNTO. */
struct rtgenmsg_local {
    unsigned char rtgen_family;
};

static int passed;
static int failed;

static void check(int condition, const char *message)
{
    if (condition) {
        ++passed;
        printf("PASS: %s\n", message);
    } else {
        ++failed;
        printf("FAIL: %s\n", message);
    }
}

static int open_route_socket(void)
{
    int fd = socket(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE);
    if (fd < 0) {
        return -1;
    }
    struct sockaddr_nl_local addr;
    memset(&addr, 0, sizeof(addr));
    addr.nl_family = AF_NETLINK;
    if (bind(fd, (struct sockaddr *)&addr, sizeof(addr)) != 0) {
        close(fd);
        return -1;
    }
    return fd;
}

static int send_request(int fd, unsigned short type, unsigned short flags, unsigned int seq)
{
    struct {
        struct nlmsghdr_local nlh;
        struct rtgenmsg_local gen;
    } request;

    memset(&request, 0, sizeof(request));
    request.nlh.nlmsg_len = NLMSG_LENGTH(sizeof(request.gen));
    request.nlh.nlmsg_type = type;
    request.nlh.nlmsg_flags = flags;
    request.nlh.nlmsg_seq = seq;
    request.gen.rtgen_family = 0;

    ssize_t sent = write(fd, &request, request.nlh.nlmsg_len);
    return sent == (ssize_t)request.nlh.nlmsg_len;
}

/*
 * Walk one datagram the way NLMSG_NEXT does and report whether the aligned
 * step ever runs past the bytes the kernel delivered. NLMSG_NEXT subtracts the
 * aligned length while NLMSG_OK compares the unaligned one, so a datagram whose
 * tail is not padded underflows the caller's remaining count; readers that keep
 * that count unsigned then walk off the buffer entirely.
 */
static int walk_is_contained(const char *buffer, size_t received, const char *label)
{
    size_t offset = 0;
    int messages = 0;
    while (received - offset >= sizeof(struct nlmsghdr_local)) {
        struct nlmsghdr_local header;
        memcpy(&header, buffer + offset, sizeof(header));
        size_t remaining = received - offset;
        if (header.nlmsg_len < sizeof(struct nlmsghdr_local) || header.nlmsg_len > remaining) {
            printf("%s: message %d declares len=%u with %zu bytes left\n", label, messages,
                   header.nlmsg_len, remaining);
            return 0;
        }
        size_t step = NLMSG_ALIGN(header.nlmsg_len);
        if (step > remaining) {
            printf("%s: message %d of type %u has len=%u, so the aligned step %zu overruns the "
                   "%zu bytes left in the datagram\n",
                   label, messages, header.nlmsg_type, header.nlmsg_len, step, remaining);
            return 0;
        }
        offset += step;
        ++messages;
    }
    if (offset != received) {
        printf("%s: %zu trailing bytes are not a message\n", label, received - offset);
        return 0;
    }
    return messages > 0;
}

static void check_reply(int type, unsigned short flags, unsigned int seq, const char *label)
{
    int fd = open_route_socket();
    if (fd < 0) {
        printf("%s: cannot open NETLINK_ROUTE socket: errno=%d (%s)\n", label, errno,
               strerror(errno));
        check(0, label);
        return;
    }
    if (!send_request(fd, (unsigned short)type, flags, seq)) {
        printf("%s: sendto failed: errno=%d (%s)\n", label, errno, strerror(errno));
        check(0, label);
        close(fd);
        return;
    }

    char buffer[8192];
    struct iovec iov;
    struct msghdr msg;
    struct sockaddr_nl_local from;

    memset(&iov, 0, sizeof(iov));
    memset(&msg, 0, sizeof(msg));
    memset(&from, 0, sizeof(from));
    iov.iov_base = buffer;
    iov.iov_len = sizeof(buffer);
    msg.msg_name = &from;
    msg.msg_namelen = sizeof(from);
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;

    ssize_t received = recvmsg(fd, &msg, 0);
    if (received < 0) {
        printf("%s: recvmsg failed: errno=%d (%s)\n", label, errno, strerror(errno));
        check(0, label);
        close(fd);
        return;
    }
    printf("%s: received %zd bytes\n", label, received);
    check(walk_is_contained(buffer, (size_t)received, label), label);
    close(fd);
}

int main(void)
{
    int probe = socket(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE);
    check(probe >= 0, "create a NETLINK_ROUTE socket");
    if (probe < 0) {
        printf("socket failed: errno=%d (%s)\n", errno, strerror(errno));
        printf("RESULT: %d passed / %d failed\n", passed, failed);
        printf("TEST FAILED\n");
        return 1;
    }
    close(probe);

    /* A request the kernel refuses is answered with NLMSG_ERROR echoing the
     * request, which is where an unpadded 17-byte echo shows up. */
    check_reply(RTM_GETROUTE, NLM_F_REQUEST, 1,
                "an error reply to an odd-length request stays inside its datagram");
    /* The multipart dump path must hold the same invariant. */
    check_reply(RTM_GETLINK, NLM_F_REQUEST | NLM_F_DUMP, 2,
                "a link dump stays inside its datagram");

    printf("RESULT: %d passed / %d failed\n", passed, failed);
    if (failed == 0) {
        printf("TEST PASSED\n");
        return 0;
    }
    printf("TEST FAILED\n");
    return 1;
}
