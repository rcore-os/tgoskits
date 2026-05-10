#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/uio.h>
#include <unistd.h>

#ifndef AF_NETLINK
#define AF_NETLINK 16
#endif

#ifndef AF_PACKET
#define AF_PACKET 17
#endif

#define NETLINK_ROUTE 0

#define NLM_F_REQUEST 0x01
#define NLM_F_MULTI 0x02
#define NLM_F_ROOT 0x100
#define NLM_F_MATCH 0x200
#define NLM_F_DUMP (NLM_F_ROOT | NLM_F_MATCH)

#define NLMSG_DONE 3

#define RTM_NEWLINK 16
#define RTM_GETLINK 18
#define RTM_NEWADDR 20
#define RTM_GETADDR 22

#define IFLA_IFNAME 3

#define IFA_ADDRESS 1
#define IFA_LOCAL 2

#define NLMSG_ALIGNTO 4U
#define NLMSG_ALIGN(len) (((len) + NLMSG_ALIGNTO - 1) & ~(NLMSG_ALIGNTO - 1))
#define NLMSG_HDRLEN ((int)NLMSG_ALIGN(sizeof(struct nlmsghdr)))
#define NLMSG_LENGTH(len) ((len) + NLMSG_HDRLEN)
#define NLMSG_SPACE(len) NLMSG_ALIGN(NLMSG_LENGTH(len))
#define NLMSG_DATA(nlh) ((void *)((char *)(nlh) + NLMSG_HDRLEN))
#define NLMSG_NEXT(nlh, len) \
    ((len) -= NLMSG_ALIGN((nlh)->nlmsg_len), \
     (struct nlmsghdr *)((char *)(nlh) + NLMSG_ALIGN((nlh)->nlmsg_len)))
#define NLMSG_OK(nlh, len) \
    ((len) >= (int)sizeof(struct nlmsghdr) && \
     (nlh)->nlmsg_len >= sizeof(struct nlmsghdr) && \
     (nlh)->nlmsg_len <= (uint32_t)(len))
#define NLMSG_PAYLOAD(nlh, len) ((nlh)->nlmsg_len - NLMSG_SPACE((len)))

#define RTA_ALIGNTO 4U
#define RTA_ALIGN(len) (((len) + RTA_ALIGNTO - 1) & ~(RTA_ALIGNTO - 1))
#define RTA_LENGTH(len) (RTA_ALIGN(sizeof(struct rtattr)) + (len))
#define RTA_DATA(rta) ((void *)((char *)(rta) + RTA_LENGTH(0)))
#define RTA_PAYLOAD(rta) ((int)((rta)->rta_len) - RTA_LENGTH(0))
#define RTA_NEXT(rta, len) \
    ((len) -= RTA_ALIGN((rta)->rta_len), \
     (struct rtattr *)((char *)(rta) + RTA_ALIGN((rta)->rta_len)))
#define RTA_OK(rta, len) \
    ((len) >= (int)sizeof(struct rtattr) && \
     (rta)->rta_len >= sizeof(struct rtattr) && \
     (rta)->rta_len <= (uint16_t)(len))

#define IFA_RTA(ifa) \
    ((struct rtattr *)((char *)(ifa) + NLMSG_ALIGN(sizeof(struct ifaddrmsg))))
#define IFA_PAYLOAD(nlh) NLMSG_PAYLOAD(nlh, sizeof(struct ifaddrmsg))
#define IFLA_RTA(ifi) \
    ((struct rtattr *)((char *)(ifi) + NLMSG_ALIGN(sizeof(struct ifinfomsg))))
#define IFLA_PAYLOAD(nlh) NLMSG_PAYLOAD(nlh, sizeof(struct ifinfomsg))

struct sockaddr_nl_compat {
    uint16_t nl_family;
    uint16_t nl_pad;
    uint32_t nl_pid;
    uint32_t nl_groups;
};

struct nlmsghdr {
    uint32_t nlmsg_len;
    uint16_t nlmsg_type;
    uint16_t nlmsg_flags;
    uint32_t nlmsg_seq;
    uint32_t nlmsg_pid;
};

struct rtgenmsg {
    unsigned char rtgen_family;
};

struct rtattr {
    uint16_t rta_len;
    uint16_t rta_type;
};

struct ifaddrmsg {
    uint8_t ifa_family;
    uint8_t ifa_prefixlen;
    uint8_t ifa_flags;
    uint8_t ifa_scope;
    uint32_t ifa_index;
};

struct ifinfomsg {
    uint8_t ifi_family;
    uint8_t __ifi_pad;
    uint16_t ifi_type;
    int32_t ifi_index;
    uint32_t ifi_flags;
    uint32_t ifi_change;
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

static int open_route_netlink(void)
{
    int fd = socket(AF_NETLINK, SOCK_DGRAM, NETLINK_ROUTE);
    check(fd >= 0, "open NETLINK_ROUTE datagram socket");
    return fd;
}

static int send_dump_request(int fd, int type, int seq, unsigned char family)
{
    struct {
        struct nlmsghdr hdr;
        struct rtgenmsg gen;
    } req;
    struct sockaddr_nl_compat kernel;
    struct iovec iov;
    struct msghdr msg;

    memset(&req, 0, sizeof(req));
    req.hdr.nlmsg_len = NLMSG_LENGTH(sizeof(req.gen));
    req.hdr.nlmsg_type = type;
    req.hdr.nlmsg_flags = NLM_F_REQUEST | NLM_F_DUMP;
    req.hdr.nlmsg_seq = seq;
    req.gen.rtgen_family = family;

    memset(&kernel, 0, sizeof(kernel));
    kernel.nl_family = AF_NETLINK;

    iov.iov_base = &req;
    iov.iov_len = req.hdr.nlmsg_len;

    memset(&msg, 0, sizeof(msg));
    msg.msg_name = &kernel;
    msg.msg_namelen = sizeof(kernel);
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;

    errno = 0;
    ssize_t sent = sendmsg(fd, &msg, 0);
    check(sent == (ssize_t)req.hdr.nlmsg_len, "send netlink dump request");
    return sent == (ssize_t)req.hdr.nlmsg_len ? 0 : -1;
}

static int attr_contains_ipv4(struct rtattr *attr, const unsigned char expected[4])
{
    return RTA_PAYLOAD(attr) >= 4 && memcmp(RTA_DATA(attr), expected, 4) == 0;
}

static int recv_addr_dump(int fd, int seq)
{
    unsigned char buf[4096];
    struct sockaddr_nl_compat peer;
    struct iovec iov;
    struct msghdr msg;
    int saw_done = 0;
    int saw_loopback = 0;
    const unsigned char loopback[4] = {127, 0, 0, 1};

    while (!saw_done) {
        memset(&peer, 0, sizeof(peer));
        iov.iov_base = buf;
        iov.iov_len = sizeof(buf);

        memset(&msg, 0, sizeof(msg));
        msg.msg_name = &peer;
        msg.msg_namelen = sizeof(peer);
        msg.msg_iov = &iov;
        msg.msg_iovlen = 1;

        ssize_t len = recvmsg(fd, &msg, 0);
        check(len > 0, "recv netlink address response");
        if (len <= 0) {
            return -1;
        }
        check(peer.nl_family == AF_NETLINK && peer.nl_pid == 0,
              "recvmsg reports kernel netlink peer");

        for (struct nlmsghdr *hdr = (struct nlmsghdr *)buf; NLMSG_OK(hdr, (unsigned int)len);
             hdr = NLMSG_NEXT(hdr, len)) {
            if (hdr->nlmsg_seq != (unsigned int)seq) {
                continue;
            }
            if (hdr->nlmsg_type == NLMSG_DONE) {
                saw_done = 1;
                break;
            }
            if (hdr->nlmsg_type != RTM_NEWADDR) {
                continue;
            }

            struct ifaddrmsg *ifa = NLMSG_DATA(hdr);
            int attr_len = IFA_PAYLOAD(hdr);
            check(ifa->ifa_family == AF_INET, "RTM_NEWADDR uses AF_INET");
            for (struct rtattr *attr = IFA_RTA(ifa); RTA_OK(attr, attr_len);
                 attr = RTA_NEXT(attr, attr_len)) {
                if (attr->rta_type == IFA_LOCAL || attr->rta_type == IFA_ADDRESS) {
                    saw_loopback |= attr_contains_ipv4(attr, loopback);
                }
            }
        }
    }

    check(saw_loopback, "RTM_GETADDR reports loopback IPv4 address");
    return saw_loopback ? 0 : -1;
}

static int recv_link_dump(int fd, int seq)
{
    unsigned char buf[4096];
    int saw_done = 0;
    int saw_lo = 0;

    while (!saw_done) {
        ssize_t len = recv(fd, buf, sizeof(buf), 0);
        check(len > 0, "recv netlink link response");
        if (len <= 0) {
            return -1;
        }

        for (struct nlmsghdr *hdr = (struct nlmsghdr *)buf; NLMSG_OK(hdr, (unsigned int)len);
             hdr = NLMSG_NEXT(hdr, len)) {
            if (hdr->nlmsg_seq != (unsigned int)seq) {
                continue;
            }
            if (hdr->nlmsg_type == NLMSG_DONE) {
                saw_done = 1;
                break;
            }
            if (hdr->nlmsg_type != RTM_NEWLINK) {
                continue;
            }

            struct ifinfomsg *ifi = NLMSG_DATA(hdr);
            int attr_len = IFLA_PAYLOAD(hdr);
            for (struct rtattr *attr = IFLA_RTA(ifi); RTA_OK(attr, attr_len);
                 attr = RTA_NEXT(attr, attr_len)) {
                if (attr->rta_type != IFLA_IFNAME) {
                    continue;
                }
                const char *name = RTA_DATA(attr);
                saw_lo |= strcmp(name, "lo") == 0;
            }
        }
    }

    check(saw_lo, "RTM_GETLINK reports loopback interface");
    return saw_lo ? 0 : -1;
}

static void check_sendmsg_regular_file(void)
{
    int fd = open("/tmp/bug_netlink_getaddr_file", O_CREAT | O_RDWR | O_TRUNC, 0600);
    check(fd >= 0, "create regular file for ENOTSOCK check");
    if (fd < 0) {
        return;
    }

    char byte = 0;
    struct iovec iov = {
        .iov_base = &byte,
        .iov_len = sizeof(byte),
    };
    struct msghdr msg = {
        .msg_iov = &iov,
        .msg_iovlen = 1,
    };

    errno = 0;
    check(sendmsg(fd, &msg, 0) == -1 && errno == ENOTSOCK,
          "sendmsg on regular file fails ENOTSOCK");
    close(fd);
    unlink("/tmp/bug_netlink_getaddr_file");
}

int main(void)
{
    check_sendmsg_regular_file();

    int fd = open_route_netlink();
    if (fd >= 0) {
        struct sockaddr_nl_compat bind_addr;
        struct sockaddr_nl_compat local;
        socklen_t local_len = sizeof(local);

        memset(&bind_addr, 0, sizeof(bind_addr));
        bind_addr.nl_family = AF_NETLINK;
        check(bind(fd, (struct sockaddr *)&bind_addr, sizeof(bind_addr)) == 0,
              "bind netlink socket with kernel-assigned pid");

        memset(&local, 0, sizeof(local));
        check(getsockname(fd, (struct sockaddr *)&local, &local_len) == 0,
              "getsockname on netlink socket succeeds");
        check(local.nl_family == AF_NETLINK && local.nl_pid != 0,
              "getsockname reports AF_NETLINK local pid");

        if (send_dump_request(fd, RTM_GETADDR, 100, AF_INET) == 0) {
            recv_addr_dump(fd, 100);
        }
        if (send_dump_request(fd, RTM_GETLINK, 101, AF_PACKET) == 0) {
            recv_link_dump(fd, 101);
        }
        close(fd);
    }

    printf("RESULT: %d passed / %d failed\n", passed, failed);
    if (failed == 0) {
        printf("TEST PASSED\n");
        return 0;
    }
    return 1;
}
