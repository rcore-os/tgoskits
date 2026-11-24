#define _GNU_SOURCE

#include <errno.h>
#include <arpa/inet.h>
#include <net/if.h>
#include <net/if_arp.h>
#include <netinet/if_ether.h>
#include <netpacket/packet.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <unistd.h>

#ifndef AF_NETLINK
#define AF_NETLINK 16
#endif

#ifndef AF_PACKET
#define AF_PACKET 17
#endif

#define NETLINK_ROUTE 0

#define NLM_F_REQUEST 0x01
#define NLM_F_ROOT 0x100
#define NLM_F_MATCH 0x200
#define NLM_F_DUMP (NLM_F_ROOT | NLM_F_MATCH)

#define NLMSG_DONE 3

#define RTM_NEWLINK 16
#define RTM_GETLINK 18

#define IFLA_IFNAME 3

#define NLMSG_ALIGNTO 4U
#define NLMSG_ALIGN(len) (((len) + NLMSG_ALIGNTO - 1) & ~(NLMSG_ALIGNTO - 1))
#define NLMSG_HDRLEN ((int)NLMSG_ALIGN(sizeof(struct nlmsghdr)))
#define NLMSG_LENGTH(len) ((len) + NLMSG_HDRLEN)
#define NLMSG_DATA(nlh) ((void *)((char *)(nlh) + NLMSG_HDRLEN))
#define NLMSG_NEXT(nlh, len) \
    ((len) -= NLMSG_ALIGN((nlh)->nlmsg_len), \
     (struct nlmsghdr *)((char *)(nlh) + NLMSG_ALIGN((nlh)->nlmsg_len)))
#define NLMSG_OK(nlh, len) \
    ((len) >= (int)sizeof(struct nlmsghdr) && \
     (nlh)->nlmsg_len >= sizeof(struct nlmsghdr) && \
     (nlh)->nlmsg_len <= (uint32_t)(len))
#define NLMSG_PAYLOAD(nlh, len) ((nlh)->nlmsg_len - NLMSG_ALIGN(NLMSG_LENGTH((len))))

#define RTA_ALIGNTO 4U
#define RTA_ALIGN(len) (((len) + RTA_ALIGNTO - 1) & ~(RTA_ALIGNTO - 1))
#define RTA_LENGTH(len) (RTA_ALIGN(sizeof(struct rtattr)) + (len))
#define RTA_DATA(rta) ((void *)((char *)(rta) + RTA_LENGTH(0)))
#define RTA_NEXT(rta, len) \
    ((len) -= RTA_ALIGN((rta)->rta_len), \
     (struct rtattr *)((char *)(rta) + RTA_ALIGN((rta)->rta_len)))
#define RTA_OK(rta, len) \
    ((len) >= (int)sizeof(struct rtattr) && \
     (rta)->rta_len >= sizeof(struct rtattr) && \
     (rta)->rta_len <= (uint16_t)(len))

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

struct ifinfomsg {
    uint8_t ifi_family;
    uint8_t __ifi_pad;
    uint16_t ifi_type;
    int32_t ifi_index;
    uint32_t ifi_flags;
    uint32_t ifi_change;
};

static int check(int condition, const char *message)
{
    if (!condition) {
        printf("FAIL: %s errno=%d (%s)\n", message, errno, strerror(errno));
        return 1;
    }
    return 0;
}

static int netlink_eth0_ifindex(void)
{
    int fd = socket(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE);
    if (fd < 0) {
        return -1;
    }

    struct sockaddr_nl_compat bind_addr;
    memset(&bind_addr, 0, sizeof(bind_addr));
    bind_addr.nl_family = AF_NETLINK;
    if (bind(fd, (struct sockaddr *)&bind_addr, sizeof(bind_addr)) != 0) {
        close(fd);
        return -1;
    }

    struct {
        struct nlmsghdr hdr;
        struct rtgenmsg gen;
    } req;
    memset(&req, 0, sizeof(req));
    req.hdr.nlmsg_len = sizeof(req);
    req.hdr.nlmsg_type = RTM_GETLINK;
    req.hdr.nlmsg_flags = NLM_F_REQUEST | NLM_F_DUMP;
    req.hdr.nlmsg_seq = 7;
    req.gen.rtgen_family = AF_PACKET;

    if (send(fd, &req, sizeof(req), 0) != (ssize_t)sizeof(req)) {
        close(fd);
        return -1;
    }

    unsigned char buf[4096];
    int eth0_ifindex = -1;
    int saw_done = 0;

    while (!saw_done && eth0_ifindex < 0) {
        ssize_t len = recv(fd, buf, sizeof(buf), 0);
        if (len <= 0) {
            close(fd);
            return -1;
        }

        for (struct nlmsghdr *hdr = (struct nlmsghdr *)buf; NLMSG_OK(hdr, (unsigned int)len);
             hdr = NLMSG_NEXT(hdr, len)) {
            if (hdr->nlmsg_seq != 7) {
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
                if (attr->rta_type == IFLA_IFNAME && strcmp((const char *)RTA_DATA(attr), "eth0") == 0) {
                    eth0_ifindex = ifi->ifi_index;
                    break;
                }
            }
        }
    }

    close(fd);
    return eth0_ifindex;
}

static int check_bytes(const unsigned char *actual, const unsigned char *expected, size_t len,
                       const char *message)
{
    if (memcmp(actual, expected, len) != 0) {
        printf("FAIL: %s\n", message);
        printf("actual:");
        for (size_t i = 0; i < len; i++) {
            printf(" %02x", actual[i]);
        }
        printf("\nexpected:");
        for (size_t i = 0; i < len; i++) {
            printf(" %02x", expected[i]);
        }
        printf("\n");
        return 1;
    }
    return 0;
}

static void fill_arp_request(unsigned char request[28], const unsigned char sender_hw[ETH_ALEN],
                             const unsigned char sender_ip[4], const unsigned char target_ip[4])
{
    memset(request, 0, 28);
    request[0] = 0x00;
    request[1] = 0x01;
    request[2] = 0x08;
    request[3] = 0x00;
    request[4] = ETH_ALEN;
    request[5] = 4;
    request[6] = 0x00;
    request[7] = 0x01;
    memcpy(&request[8], sender_hw, ETH_ALEN);
    memcpy(&request[14], sender_ip, 4);
    memcpy(&request[24], target_ip, 4);
}

static int expect_no_packet(int fd, const char *message)
{
    unsigned char reply[64];
    struct sockaddr_ll from;
    socklen_t from_len = sizeof(from);
    errno = 0;
    ssize_t got = recvfrom(fd, reply, sizeof(reply), 0, (struct sockaddr *)&from, &from_len);
    if (got >= 0) {
        printf("FAIL: %s unexpectedly received %zd bytes\n", message, got);
        return 1;
    }
    if (errno != EAGAIN && errno != EWOULDBLOCK) {
        printf("FAIL: %s errno=%d (%s)\n", message, errno, strerror(errno));
        return 1;
    }
    return 0;
}

int main(void)
{
    printf("=== bug-packet-arping ===\n");

    int fd = socket(AF_PACKET, SOCK_DGRAM, 0);
    if (fd < 0 && (errno == EPERM || errno == EACCES)) {
        printf("SKIP: AF_PACKET requires CAP_NET_RAW on Linux\n");
        printf("TEST PASSED\n");
        return 0;
    }
    if (check(fd >= 0, "socket(AF_PACKET, SOCK_DGRAM, 0)")) {
        return 1;
    }

    struct ifreq ifr;
    memset(&ifr, 0, sizeof(ifr));
    strncpy(ifr.ifr_name, "eth0", IFNAMSIZ - 1);
    if (check(ioctl(fd, SIOCGIFINDEX, &ifr) == 0, "SIOCGIFINDEX eth0")) {
        close(fd);
        return 1;
    }
    int ifindex = ifr.ifr_ifindex;
    printf("eth0 ifindex=%d\n", ifindex);

    int netlink_ifindex = netlink_eth0_ifindex();
    printf("eth0 netlink ifindex=%d\n", netlink_ifindex);
    if (check(netlink_ifindex == ifindex, "SIOCGIFINDEX eth0 matches RTM_GETLINK eth0")) {
        close(fd);
        return 1;
    }

    memset(&ifr, 0, sizeof(ifr));
    strncpy(ifr.ifr_name, "eth0", IFNAMSIZ - 1);
    if (check(ioctl(fd, SIOCGIFFLAGS, &ifr) == 0, "SIOCGIFFLAGS eth0")) {
        close(fd);
        return 1;
    }
    if (check((ifr.ifr_flags & IFF_UP) != 0, "eth0 should be up")) {
        close(fd);
        return 1;
    }
    if (check((ifr.ifr_flags & (IFF_LOOPBACK | IFF_NOARP)) == 0, "eth0 should be ARPable")) {
        close(fd);
        return 1;
    }

    struct sockaddr_ll local;
    memset(&local, 0, sizeof(local));
    local.sll_family = AF_PACKET;
    local.sll_protocol = htons(ETH_P_ARP);
    local.sll_ifindex = ifindex;
    if (check(bind(fd, (struct sockaddr *)&local, sizeof(local)) == 0, "bind AF_PACKET eth0")) {
        close(fd);
        return 1;
    }

    socklen_t local_len = sizeof(local);
    memset(&local, 0, sizeof(local));
    if (check(getsockname(fd, (struct sockaddr *)&local, &local_len) == 0, "getsockname AF_PACKET")) {
        close(fd);
        return 1;
    }
    if (check(local.sll_hatype == ARPHRD_ETHER && local.sll_halen == ETH_ALEN, "ethernet lladdr")) {
        close(fd);
        return 1;
    }

    int nonblock = 1;
    if (check(ioctl(fd, FIONBIO, &nonblock) == 0, "FIONBIO packet socket")) {
        close(fd);
        return 1;
    }

    const unsigned char eth0_ip[4] = {10, 0, 2, 15};
    const unsigned char gateway_ip[4] = {10, 0, 2, 2};
    const unsigned char loopback_ip[4] = {127, 0, 0, 1};
    const unsigned char unknown_ip[4] = {10, 0, 2, 254};
    unsigned char request[28];
    fill_arp_request(request, local.sll_addr, eth0_ip, gateway_ip);

    struct sockaddr_ll peer = local;
    memset(peer.sll_addr, 0xff, ETH_ALEN);
    ssize_t sent = sendto(fd, request, sizeof(request), 0, (struct sockaddr *)&peer, sizeof(peer));
    if (check(sent == (ssize_t)sizeof(request), "sendto AF_PACKET ARP request for modeled gateway")) {
        close(fd);
        return 1;
    }

    unsigned char reply[64];
    struct sockaddr_ll from;
    socklen_t from_len = sizeof(from);
    ssize_t got = recvfrom(fd, reply, sizeof(reply), 0, (struct sockaddr *)&from, &from_len);
    if (check(got >= 28, "modeled gateway AF_PACKET ARP reply")) {
        close(fd);
        return 1;
    }
    if (check(reply[6] == 0x00 && reply[7] == 0x02, "ARP reply opcode")) {
        close(fd);
        return 1;
    }
    const unsigned char synthetic_peer_hwaddr[ETH_ALEN] = {0x02, 0x00, 0x00, 0x00, 0x00, 0x02};
    if (check_bytes(&reply[8], synthetic_peer_hwaddr, ETH_ALEN, "ARP reply sender hardware address")) {
        close(fd);
        return 1;
    }
    if (check_bytes(&reply[14], gateway_ip, 4, "ARP reply sender protocol address")) {
        close(fd);
        return 1;
    }
    if (check_bytes(&reply[18], &request[8], ETH_ALEN, "ARP reply target hardware address")) {
        close(fd);
        return 1;
    }
    if (check_bytes(&reply[24], &request[14], 4, "ARP reply target protocol address")) {
        close(fd);
        return 1;
    }
    if (check(from.sll_hatype == ARPHRD_ETHER && from.sll_halen == ETH_ALEN, "reply sockaddr_ll")) {
        close(fd);
        return 1;
    }

    fill_arp_request(request, local.sll_addr, eth0_ip, loopback_ip);
    sent = sendto(fd, request, sizeof(request), 0, (struct sockaddr *)&peer, sizeof(peer));
    if (check(sent == (ssize_t)sizeof(request), "sendto AF_PACKET ARP request for loopback")) {
        close(fd);
        return 1;
    }
    if (expect_no_packet(fd, "loopback ARP target must not receive a reply")) {
        close(fd);
        return 1;
    }

    fill_arp_request(request, local.sll_addr, eth0_ip, unknown_ip);
    sent = sendto(fd, request, sizeof(request), 0, (struct sockaddr *)&peer, sizeof(peer));
    if (check(sent == (ssize_t)sizeof(request), "sendto AF_PACKET ARP request for unknown peer")) {
        close(fd);
        return 1;
    }
    if (expect_no_packet(fd, "unknown ARP target must not receive a reply")) {
        close(fd);
        return 1;
    }

    close(fd);
    printf("packet ARP socket only models the configured gateway peer\n");
    printf("TEST PASSED\n");
    return 0;
}
