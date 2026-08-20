/*
 * StarryOS VTP server.
 *
 * Runs the Starry side of the VTP demo: configures eth0 with a static IPv4
 * address via netlink RTM_NEWADDR (Starry does not implement SIOCSIFADDR) and
 * verifies by reading the address back, then acts as the VTP controller that
 * requests STATUS from the FreeRTOS agent, exchanges DATA both ways, and
 * reports decode errors. Prints STARRY_VTP_PASS when the demo handshake
 * completes, STARRY_VTP_FAIL on timeout.
 *
 * Build integration: test-suit/starryos/qemu/system/axvisor-vtp-server/
 * (CMake installs the binary into the Starry rootfs at /usr/bin/axvisor-vtp-server).
 * The shared codec is test-suit/axvisor/normal/qemu-vtp/protocol/vtp.{h,c}.
 */

#include <arpa/inet.h>
#include <errno.h>
#include <net/if.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

/* CI containers lack linux-libc-dev; the netlink UAPI used here is restated in
 * this self-contained header instead of including <linux/netlink.h>. */
#include "netlink_compat.h"

#include "vtp.h"

#define VTP_PORT 6000
#define LOCAL_IP "10.0.2.15"
#define PEER_IP "10.0.2.16"
#define NETMASK_PREFIX 24

#define REQUIRED_STATUS_ROUNDS 5
#define SEND_INTERVAL_MS 1000
#define RUN_MS 90000
#define RECV_TIMEOUT_MS 500

static uint32_t now_ms(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint32_t)((uint64_t)ts.tv_sec * 1000u + (uint64_t)ts.tv_nsec / 1000000u);
}

/* ------------------------------------------------------------------ */
/* Interface configuration via netlink RTM_NEWADDR + verify.          */
/* ------------------------------------------------------------------ */

static int get_ifindex(const char *ifname)
{
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    struct ifreq ifr;
    int rc;

    if (fd < 0) {
        return -1;
    }
    memset(&ifr, 0, sizeof(ifr));
    strncpy(ifr.ifr_name, ifname, IFNAMSIZ - 1);
    rc = ioctl(fd, SIOCGIFINDEX, &ifr);
    close(fd);
    if (rc < 0) {
        return -1;
    }
    return ifr.ifr_ifindex;
}

/* Read the current IPv4 address of an interface via SIOCGIFADDR. */
static int get_iface_ipv4(const char *ifname, uint32_t *out4)
{
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    struct ifreq ifr;
    struct sockaddr_in *sin;
    int rc;

    if (fd < 0) {
        return -1;
    }
    memset(&ifr, 0, sizeof(ifr));
    strncpy(ifr.ifr_name, ifname, IFNAMSIZ - 1);
    rc = ioctl(fd, SIOCGIFADDR, &ifr);
    close(fd);
    if (rc < 0) {
        return -1;
    }
    sin = (struct sockaddr_in *)&ifr.ifr_addr;
    *out4 = sin->sin_addr.s_addr;
    return 0;
}

/* Send RTM_NEWADDR to set a static IPv4 address, then verify by reading the
 * address back. Verifying avoids depending on a netlink ACK reply. */
static int set_iface_ipv4(int ifindex, const char *ip, int prefix)
{
    uint8_t buf[512];
    struct nlmsghdr *nlh = (struct nlmsghdr *)buf;
    struct ifaddrmsg *ifa;
    struct nlattr *attr;
    uint32_t addr4;
    uint8_t *p;
    int fd;
    ssize_t sent;
    uint32_t check4;

    if (inet_pton(AF_INET, ip, &addr4) != 1) {
        return -1;
    }

    memset(buf, 0, sizeof(buf));
    nlh->nlmsg_type = RTM_NEWADDR;
    nlh->nlmsg_flags = NLM_F_REQUEST | NLM_F_ACK | NLM_F_CREATE | NLM_F_REPLACE;
    nlh->nlmsg_seq = (uint32_t)getpid();
    nlh->nlmsg_len = NLMSG_LENGTH(sizeof(struct ifaddrmsg));

    ifa = (struct ifaddrmsg *)NLMSG_DATA(nlh);
    ifa->ifa_family = AF_INET;
    ifa->ifa_prefixlen = (uint8_t)prefix;
    ifa->ifa_flags = IFA_F_PERMANENT;
    ifa->ifa_scope = RT_SCOPE_UNIVERSE;
    ifa->ifa_index = (int32_t)ifindex;

    /* IFA_LOCAL and IFA_ADDRESS (identical for a non-point-to-point netif). */
    p = buf + NLMSG_LENGTH(sizeof(struct ifaddrmsg));
    attr = (struct nlattr *)p;
    attr->nla_len = NLA_HDRLEN + 4;
    attr->nla_type = IFA_LOCAL;
    memcpy((uint8_t *)attr + NLA_HDRLEN, &addr4, 4);
    p += NLA_ALIGN(attr->nla_len);

    attr = (struct nlattr *)p;
    attr->nla_len = NLA_HDRLEN + 4;
    attr->nla_type = IFA_ADDRESS;
    memcpy((uint8_t *)attr + NLA_HDRLEN, &addr4, 4);
    p += NLA_ALIGN(attr->nla_len);
    nlh->nlmsg_len = (uint32_t)(p - buf);

    fd = socket(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE);
    if (fd < 0) {
        return -1;
    }
    {
        struct sockaddr_nl sa;
        memset(&sa, 0, sizeof(sa));
        sa.nl_family = AF_NETLINK;
        sent = sendto(fd, buf, nlh->nlmsg_len, 0, (struct sockaddr *)&sa, sizeof(sa));
    }
    close(fd);
    if (sent < 0) {
        return -1;
    }

    /* Verify: the address must now read back as requested. */
    if (get_iface_ipv4("eth0", &check4) < 0) {
        return -1;
    }
    return (check4 == addr4) ? 0 : -1;
}

/* ------------------------------------------------------------------ */
/* VTP message handlers                                               */
/* ------------------------------------------------------------------ */

static void send_error(int fd, const struct sockaddr_in *peer, vtp_peer_t *vtp,
                       uint16_t error_code, const char *detail)
{
    uint8_t wire[512];
    int n = vtp_encode_error(wire, sizeof(wire), VTP_FLAG_REQUEST, vtp_tx_seq(vtp),
                             now_ms(), error_code, 0x53 /* source = Starry */,
                             (const uint8_t *)detail, (uint8_t)strlen(detail));
    if (n > 0) {
        sendto(fd, wire, (size_t)n, 0, (const struct sockaddr *)peer, sizeof(*peer));
    }
}

/* Respond to a CONTROL from the peer (role-agnostic responder). */
static int handle_control(int fd, const struct sockaddr_in *peer, vtp_peer_t *vtp,
                          uint8_t cmd, uint8_t req_seq, const uint8_t *data,
                          uint8_t data_len)
{
    uint8_t wire[512];
    int n;

    (void)data;
    (void)data_len;
    switch (cmd) {
    case VTP_CMD_PING:
    case VTP_CMD_SET_STATE:
    case VTP_CMD_RESET:
        n = vtp_encode_ack(wire, sizeof(wire), req_seq, now_ms(), 1, VTP_ERR_OK);
        break;
    default:
        n = vtp_encode_error(wire, sizeof(wire), VTP_FLAG_REQUEST, vtp_tx_seq(vtp),
                             now_ms(), VTP_ERR_UNKNOWN_CMD, 0x53, (const uint8_t *)"cmd",
                             3);
        break;
    }
    if (n > 0) {
        sendto(fd, wire, (size_t)n, 0, (const struct sockaddr *)peer, sizeof(*peer));
    }
    return 0;
}

int main(void)
{
    struct sockaddr_in bind_addr;
    struct sockaddr_in peer_addr;
    struct sockaddr_in from;
    uint8_t wire[VTP_HEADER_LEN + VTP_MAX_PAYLOAD];
    vtp_peer_t vtp;
    int status_rounds = 0;
    int saw_data = 0;
    int control_acked = 0;
    int control_sent = 0;
    int fd;
    uint32_t start_ms;
    uint32_t last_send_ms;
    int ifindex;

    printf("STARRY_VTP_READY ip=%s peer=%s\n", LOCAL_IP, PEER_IP);

    ifindex = get_ifindex("eth0");
    if (ifindex < 0) {
        printf("STARRY_VTP_FAIL eth0 not found\n");
        return 1;
    }
    if (set_iface_ipv4(ifindex, LOCAL_IP, NETMASK_PREFIX) < 0) {
        printf("STARRY_VTP_FAIL cannot set %s on eth0\n", LOCAL_IP);
        return 1;
    }
    printf("STARRY_VTP_IP eth0=%s/%d ifindex=%d\n", LOCAL_IP, NETMASK_PREFIX, ifindex);

    fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) {
        printf("STARRY_VTP_FAIL socket\n");
        return 1;
    }

    memset(&bind_addr, 0, sizeof(bind_addr));
    bind_addr.sin_family = AF_INET;
    bind_addr.sin_port = htons(VTP_PORT);
    bind_addr.sin_addr.s_addr = htonl(INADDR_ANY);
    if (bind(fd, (struct sockaddr *)&bind_addr, sizeof(bind_addr)) < 0) {
        printf("STARRY_VTP_FAIL bind\n");
        return 1;
    }

    memset(&peer_addr, 0, sizeof(peer_addr));
    peer_addr.sin_family = AF_INET;
    peer_addr.sin_port = htons(VTP_PORT);
    inet_pton(AF_INET, PEER_IP, &peer_addr.sin_addr);

    vtp_peer_init(&vtp, 1);
    start_ms = now_ms();
    last_send_ms = start_ms;

    for (;;) {
        uint32_t now = now_ms();
        if (now - start_ms >= RUN_MS) {
            printf("STARRY_VTP_FAIL rounds=%d data=%d acked=%d sent=%d\n",
                   status_rounds, saw_data, control_acked, control_sent);
            return 1;
        }

        /* Every second: drive the peer. */
        if (now - last_send_ms >= SEND_INTERVAL_MS) {
            char data[64];
            int n;

            /* CONTROL REQ_STATUS with ACK_REQUESTED. */
            n = vtp_encode_control(wire, sizeof(wire), VTP_FLAG_REQUEST | VTP_FLAG_ACK_REQUESTED,
                                   vtp_tx_seq(&vtp), now, VTP_CMD_REQ_STATUS, NULL, 0);
            if (n > 0) {
                sendto(fd, wire, (size_t)n, 0, (struct sockaddr *)&peer_addr,
                       sizeof(peer_addr));
            }
            control_sent++;

            /* Bidirectional DATA. */
            snprintf(data, sizeof(data), "starry-data-%u", vtp_tx_seq(&vtp));
            n = vtp_encode_data(wire, sizeof(wire), VTP_FLAG_REQUEST, vtp_tx_seq(&vtp),
                                now, (const uint8_t *)data, (uint16_t)strlen(data));
            if (n > 0) {
                sendto(fd, wire, (size_t)n, 0, (struct sockaddr *)&peer_addr,
                       sizeof(peer_addr));
            }
            last_send_ms = now;
        }

        /* Receive one datagram with a bounded wait (select instead of
         * SO_RCVTIMEO: select() is more widely implemented by Starry). */
        {
            fd_set rfds;
            struct timeval tv;

            FD_ZERO(&rfds);
            FD_SET(fd, &rfds);
            tv.tv_sec = 0;
            tv.tv_usec = RECV_TIMEOUT_MS * 1000;
            int ready = select(fd + 1, &rfds, NULL, NULL, &tv);
            if (ready < 0) {
                printf("STARRY_VTP_FAIL select errno=%d\n", errno);
                return 1;
            }
            if (ready == 0 || !FD_ISSET(fd, &rfds)) {
                continue;
            }
        }
        {
            socklen_t from_len = sizeof(from);
            ssize_t n = recvfrom(fd, wire, sizeof(wire), 0, (struct sockaddr *)&from,
                                 &from_len);
            if (n < 0) {
                if (errno == EAGAIN || errno == EWOULDBLOCK) {
                    continue;
                }
                printf("STARRY_VTP_FAIL recvfrom errno=%d\n", errno);
                return 1;
            }
            if (n < (ssize_t)VTP_HEADER_LEN) {
                continue;
            }

            vtp_header_t hdr;
            const uint8_t *payload;
            uint16_t payload_len;
            int rc = vtp_decode(wire, (size_t)n, &hdr, &payload, &payload_len);
            if (rc < 0) {
                /* Decode failure → notify peer via ERROR. */
                printf("STARRY_VTP_ERROR decode rc=%d\n", rc);
                send_error(fd, &from, &vtp, (uint16_t)(-rc), "badframe");
                continue;
            }

            switch (hdr.msg_type) {
            case VTP_MSG_STATUS:
                status_rounds++;
                printf("STARRY_VTP_STATUS seq=%u rounds=%d\n", hdr.seq, status_rounds);
                break;
            case VTP_MSG_DATA:
                saw_data = 1;
                printf("STARRY_VTP_DATA seq=%u len=%u\n", hdr.seq, payload_len);
                break;
            case VTP_MSG_ACK:
                control_acked = 1;
                break;
            case VTP_MSG_ERROR: {
                uint16_t ec;
                uint8_t source;
                const uint8_t *detail;
                uint8_t detail_len;
                if (vtp_parse_error(payload, payload_len, &ec, &source, &detail,
                                    &detail_len) == VTP_ERR_OK) {
                    printf("STARRY_VTP_ERROR_NOTIFY code=0x%x source=%u\n", ec, source);
                }
                break;
            }
            case VTP_MSG_CONTROL: {
                uint8_t cmd;
                const uint8_t *data;
                uint8_t data_len;
                if (vtp_parse_control(payload, payload_len, &cmd, &data, &data_len) ==
                    VTP_ERR_OK) {
                    handle_control(fd, &from, &vtp, cmd, hdr.seq, data, data_len);
                }
                break;
            }
            default:
                break;
            }

            if (status_rounds >= REQUIRED_STATUS_ROUNDS && saw_data && control_acked) {
                printf("STARRY_VTP_PASS rounds=%d data=%d acked=%d\n", status_rounds,
                       saw_data, control_acked);
                return 0;
            }
        }
    }
}
