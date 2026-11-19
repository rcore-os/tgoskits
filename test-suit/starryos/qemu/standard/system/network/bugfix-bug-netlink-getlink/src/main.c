#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <sys/uio.h>
#include <unistd.h>

#ifndef AF_NETLINK
#define AF_NETLINK 16
#endif
#ifndef SIOCGIFTXQLEN
#define SIOCGIFTXQLEN 0x8942
#endif

#define NETLINK_ROUTE 0
#define NLM_F_REQUEST 0x01
#define NLM_F_DUMP 0x300
#define NLMSG_DONE 3
#define RTM_GETLINK 18
#define RTM_NEWLINK 16
#define IFLA_ADDRESS 1
#define IFLA_IFNAME 3
#ifndef AF_PACKET
#define AF_PACKET 17
#endif
#define IFNAMSIZ 16

#define NLMSG_ALIGNTO 4U
#define NLMSG_ALIGN(len) (((len) + NLMSG_ALIGNTO - 1) & ~(NLMSG_ALIGNTO - 1))
#define NLMSG_HDRLEN ((int)NLMSG_ALIGN(sizeof(struct nlmsghdr_local)))
#define NLMSG_LENGTH(len) ((len) + NLMSG_HDRLEN)
#define NLMSG_DATA(nlh) ((void *)((char *)(nlh) + NLMSG_LENGTH(0)))
#define NLMSG_NEXT(nlh, len)                                                                    \
    ((len) -= NLMSG_ALIGN((nlh)->nlmsg_len),                                                    \
     (struct nlmsghdr_local *)((char *)(nlh) + NLMSG_ALIGN((nlh)->nlmsg_len)))
#define NLMSG_OK(nlh, len)                                                                       \
    ((len) >= (int)sizeof(struct nlmsghdr_local) && (nlh)->nlmsg_len >= sizeof(struct nlmsghdr_local) && \
     (nlh)->nlmsg_len <= (unsigned int)(len))

#define RTA_ALIGNTO 4U
#define RTA_ALIGN(len) (((len) + RTA_ALIGNTO - 1) & ~(RTA_ALIGNTO - 1))
#define RTA_LENGTH(len) (RTA_ALIGN(sizeof(struct rtattr_local)) + (len))
#define RTA_DATA(rta) ((void *)((char *)(rta) + RTA_LENGTH(0)))
#define RTA_PAYLOAD(rta) ((int)((rta)->rta_len) - RTA_LENGTH(0))
#define RTA_NEXT(rta, attrlen)                                                                   \
    ((attrlen) -= RTA_ALIGN((rta)->rta_len),                                                     \
     (struct rtattr_local *)((char *)(rta) + RTA_ALIGN((rta)->rta_len)))
#define RTA_OK(rta, len)                                                                         \
    ((len) >= (int)sizeof(struct rtattr_local) && (rta)->rta_len >= sizeof(struct rtattr_local) && \
     (rta)->rta_len <= (unsigned int)(len))
#define IFLA_RTA(r) ((struct rtattr_local *)(((char *)(r)) + NLMSG_ALIGN(sizeof(struct ifinfomsg_local))))

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

struct rtgenmsg_local {
    unsigned char rtgen_family;
};

struct ifinfomsg_local {
    unsigned char ifi_family;
    unsigned char __ifi_pad;
    unsigned short ifi_type;
    int ifi_index;
    unsigned int ifi_flags;
    unsigned int ifi_change;
};

struct rtattr_local {
    unsigned short rta_len;
    unsigned short rta_type;
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

static void expect_enotsock(const char *message, ssize_t result, int saved_errno)
{
    if (result != -1 || saved_errno != ENOTSOCK) {
        printf("unexpected result for %s: result=%ld errno=%d (%s)\n", message, (long)result,
               saved_errno, strerror(saved_errno));
    }
    check(result == -1 && saved_errno == ENOTSOCK, message);
}

static ssize_t sendmsg_noaddr(int fd)
{
    char byte = 'x';
    struct iovec iov;
    struct msghdr msg;

    memset(&iov, 0, sizeof(iov));
    memset(&msg, 0, sizeof(msg));
    iov.iov_base = &byte;
    iov.iov_len = sizeof(byte);
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;

    return sendmsg(fd, &msg, 0);
}

static ssize_t recvmsg_noaddr(int fd)
{
    char byte;
    struct iovec iov;
    struct msghdr msg;

    memset(&iov, 0, sizeof(iov));
    memset(&msg, 0, sizeof(msg));
    iov.iov_base = &byte;
    iov.iov_len = sizeof(byte);
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;

    return recvmsg(fd, &msg, 0);
}

static void check_regular_file_enotsock(void)
{
    const char *path = "/tmp/bug-netlink-getlink-nonsocket";
    char byte = 'x';
    ssize_t result;
    int saved_errno;
    int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0600);
    check(fd >= 0, "create regular file for non-socket errno checks");
    if (fd < 0) {
        return;
    }

    check(write(fd, "abcdef", 6) == 6, "prime regular file for recv errno checks");

    errno = 0;
    result = send(fd, &byte, sizeof(byte), 0);
    saved_errno = errno;
    expect_enotsock("send on regular file fails with ENOTSOCK", result, saved_errno);

    errno = 0;
    result = sendmsg_noaddr(fd);
    saved_errno = errno;
    expect_enotsock("sendmsg on regular file fails with ENOTSOCK", result, saved_errno);

    check(lseek(fd, 0, SEEK_SET) == 0, "rewind regular file before recv");
    errno = 0;
    result = recv(fd, &byte, sizeof(byte), 0);
    saved_errno = errno;
    expect_enotsock("recv on regular file fails with ENOTSOCK", result, saved_errno);

    check(lseek(fd, 0, SEEK_SET) == 0, "rewind regular file before recvmsg");
    errno = 0;
    result = recvmsg_noaddr(fd);
    saved_errno = errno;
    expect_enotsock("recvmsg on regular file fails with ENOTSOCK", result, saved_errno);

    close(fd);
    unlink(path);
}

static void check_pipe_enotsock(void)
{
    char byte = 'x';
    ssize_t result;
    int saved_errno;
    int fds[2] = {-1, -1};

    check(pipe(fds) == 0, "create pipe for non-socket errno checks");
    if (fds[0] < 0 || fds[1] < 0) {
        return;
    }

    errno = 0;
    result = send(fds[1], &byte, sizeof(byte), 0);
    saved_errno = errno;
    expect_enotsock("send on pipe fd fails with ENOTSOCK", result, saved_errno);

    errno = 0;
    result = sendmsg_noaddr(fds[1]);
    saved_errno = errno;
    expect_enotsock("sendmsg on pipe fd fails with ENOTSOCK", result, saved_errno);

    check(write(fds[1], &byte, sizeof(byte)) == (ssize_t)sizeof(byte),
          "prime pipe before recv");
    errno = 0;
    result = recv(fds[0], &byte, sizeof(byte), 0);
    saved_errno = errno;
    expect_enotsock("recv on pipe fd fails with ENOTSOCK", result, saved_errno);

    check(write(fds[1], &byte, sizeof(byte)) == (ssize_t)sizeof(byte),
          "prime pipe before recvmsg");
    errno = 0;
    result = recvmsg_noaddr(fds[0]);
    saved_errno = errno;
    expect_enotsock("recvmsg on pipe fd fails with ENOTSOCK", result, saved_errno);

    close(fds[0]);
    close(fds[1]);
}

static int request_getlink(int fd)
{
    struct {
        struct nlmsghdr_local nh;
        struct rtgenmsg_local gen;
    } req;

    memset(&req, 0, sizeof(req));
    req.nh.nlmsg_len = sizeof(req);
    req.nh.nlmsg_type = RTM_GETLINK;
    req.nh.nlmsg_flags = NLM_F_REQUEST | NLM_F_DUMP;
    req.nh.nlmsg_seq = 7;
    req.gen.rtgen_family = AF_PACKET;

    return write(fd, &req, sizeof(req)) == (ssize_t)sizeof(req);
}

static void inspect_links(int fd, unsigned int local_pid)
{
    char buf[4096];
    int saw_link = 0;
    int saw_name = 0;
    int saw_addr = 0;
    int saw_done = 0;
    int saw_kernel_peer = 0;
    int saw_local_port = 0;

    while (!saw_done) {
        struct sockaddr_nl_local peer;
        struct iovec iov;
        struct msghdr msg;

        memset(&peer, 0, sizeof(peer));
        memset(&iov, 0, sizeof(iov));
        memset(&msg, 0, sizeof(msg));
        iov.iov_base = buf;
        iov.iov_len = sizeof(buf);
        msg.msg_name = &peer;
        msg.msg_namelen = sizeof(peer);
        msg.msg_iov = &iov;
        msg.msg_iovlen = 1;

        ssize_t nread = recvmsg(fd, &msg, 0);
        if (nread < 0) {
            printf("recvmsg failed: errno=%d (%s)\n", errno, strerror(errno));
            break;
        }
        if (msg.msg_namelen >= sizeof(peer) && peer.nl_family == AF_NETLINK &&
            peer.nl_pid == 0) {
            saw_kernel_peer = 1;
        }

        int remaining = (int)nread;
        for (struct nlmsghdr_local *nh = (struct nlmsghdr_local *)buf; NLMSG_OK(nh, remaining);
             nh = NLMSG_NEXT(nh, remaining)) {
            if (nh->nlmsg_type == NLMSG_DONE) {
                if (nh->nlmsg_pid == local_pid) {
                    saw_local_port = 1;
                }
                saw_done = 1;
                break;
            }
            if (nh->nlmsg_type != RTM_NEWLINK) {
                continue;
            }
            if (nh->nlmsg_pid == local_pid) {
                saw_local_port = 1;
            }

            saw_link = 1;
            struct ifinfomsg_local *ifi = NLMSG_DATA(nh);
            int len = nh->nlmsg_len - NLMSG_LENGTH(sizeof(*ifi));
            for (struct rtattr_local *attr = IFLA_RTA(ifi); RTA_OK(attr, len);
                 attr = RTA_NEXT(attr, len)) {
                if (attr->rta_type == IFLA_IFNAME && strcmp(RTA_DATA(attr), "lo") == 0) {
                    saw_name = 1;
                }
                if (attr->rta_type == IFLA_ADDRESS && RTA_PAYLOAD(attr) >= 6) {
                    saw_addr = 1;
                }
            }
        }
    }

    check(saw_link, "RTM_GETLINK returns at least one RTM_NEWLINK message");
    check(saw_name, "RTM_GETLINK exposes the loopback interface name");
    check(saw_addr, "RTM_GETLINK exposes a link-layer address attribute");
    check(saw_kernel_peer, "recvmsg reports the kernel netlink peer");
    check(saw_local_port, "netlink headers target the bound local port id");
    check(saw_done, "RTM_GETLINK multipart dump ends with NLMSG_DONE");
}

static void check_tx_queue_len(void)
{
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    check(fd >= 0, "create AF_INET socket for SIOCGIFTXQLEN");
    if (fd < 0) {
        return;
    }

    char ifr[40];
    int qlen;

    memset(ifr, 0, sizeof(ifr));
    strncpy(ifr, "lo", IFNAMSIZ - 1);

    check(ioctl(fd, SIOCGIFTXQLEN, ifr) == 0, "SIOCGIFTXQLEN succeeds on lo");
    memcpy(&qlen, ifr + IFNAMSIZ, sizeof(qlen));
    check(qlen >= 0, "SIOCGIFTXQLEN returns a non-negative queue length");
    close(fd);
}

int main(void)
{
    int fd = socket(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE);
    check(fd >= 0, "create NETLINK_ROUTE socket");
    if (fd < 0) {
        printf("socket failed: errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }

    struct sockaddr_nl_local addr;
    memset(&addr, 0, sizeof(addr));
    addr.nl_family = AF_NETLINK;
    check(bind(fd, (struct sockaddr *)&addr, sizeof(addr)) == 0, "bind NETLINK_ROUTE socket");

    socklen_t addrlen = sizeof(addr);
    memset(&addr, 0, sizeof(addr));
    check(getsockname(fd, (struct sockaddr *)&addr, &addrlen) == 0,
          "getsockname returns netlink sockaddr");
    check(addr.nl_family == AF_NETLINK, "getsockname reports AF_NETLINK");

    check(request_getlink(fd), "send RTM_GETLINK dump request");
    inspect_links(fd, addr.nl_pid);
    close(fd);

    check_tx_queue_len();
    check_regular_file_enotsock();
    check_pipe_enotsock();

    printf("RESULT: %d passed / %d failed\n", passed, failed);
    if (failed == 0) {
        printf("TEST PASSED\n");
        return 0;
    }
    printf("TEST FAILED\n");
    return 1;
}
