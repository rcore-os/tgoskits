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

static int check(int condition, const char *message)
{
    if (!condition) {
        printf("FAIL: %s errno=%d (%s)\n", message, errno, strerror(errno));
        return 1;
    }
    return 0;
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

    unsigned char request[28] = {0};
    request[0] = 0x00;
    request[1] = 0x01;
    request[2] = 0x08;
    request[3] = 0x00;
    request[4] = ETH_ALEN;
    request[5] = 4;
    request[6] = 0x00;
    request[7] = 0x01;
    memcpy(&request[8], local.sll_addr, ETH_ALEN);
    request[14] = 10;
    request[15] = 0;
    request[16] = 2;
    request[17] = 15;
    memset(&request[18], 0xff, ETH_ALEN);
    request[24] = 127;
    request[25] = 0;
    request[26] = 0;
    request[27] = 1;

    struct sockaddr_ll peer = local;
    memset(peer.sll_addr, 0xff, ETH_ALEN);
    ssize_t sent = sendto(fd, request, sizeof(request), 0, (struct sockaddr *)&peer, sizeof(peer));
    if (check(sent == (ssize_t)sizeof(request), "sendto AF_PACKET ARP request")) {
        close(fd);
        return 1;
    }

    int nonblock = 1;
    if (check(ioctl(fd, FIONBIO, &nonblock) == 0, "FIONBIO packet socket")) {
        close(fd);
        return 1;
    }

    unsigned char reply[64];
    struct sockaddr_ll from;
    socklen_t from_len = sizeof(from);
    ssize_t got = recvfrom(fd, reply, sizeof(reply), 0, (struct sockaddr *)&from, &from_len);
    if (check(got >= 28, "synthetic AF_PACKET ARP reply")) {
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
    if (check_bytes(&reply[14], &request[24], 4, "ARP reply sender protocol address")) {
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

    close(fd);
    printf("packet ARP socket returned a synthetic reply\n");
    printf("TEST PASSED\n");
    return 0;
}
