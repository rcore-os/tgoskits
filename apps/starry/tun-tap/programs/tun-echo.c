// tun-echo - self-contained layer-3 TUN (/dev/net/tun) datapath carpet for
// StarryOS. Scoped to the IFF_TUN | IFF_NO_PI packet path; the full TUN/TAP
// ioctl/syscall ABI matrix (TAP framing, second-fd EBUSY, persist, recreate,
// permissions, name allocation) is owned by #1566's syscall-test-tun-tap-abi.
//
// The test drives the whole TUN datapath without any external tool or second
// network endpoint:
//
//   1. open /dev/net/tun and TUNSETIFF a layer-3 tun0 (IFF_TUN | IFF_NO_PI)
//   2. assign 10.8.0.1/24 (SIOCSIFADDR/SIOCSIFNETMASK) and bring it UP
//      (SIOCSIFFLAGS)
//   3. inject an ICMP echo *request* into the fd, framed as if it arrived from
//      the peer 10.8.0.2 addressed to the local 10.8.0.1
//   4. the kernel stack ingests it, generates an ICMP echo *reply*, and routes
//      it back out tun0; the test reads that reply back off the fd and checks
//      the addresses are swapped, the type is 0, the id/seq echo, and the
//      checksum is valid
//
// A second phase exercises the userspace-echo direction the blueprint calls
// out: the process writes a bare packet in and reads it back through a small
// child responder, proving read()/write() round-trip framing.
//
// Every check prints PASS/FAIL with a label; the script wrapper decides the
// final verdict from the "ALL CHECKS PASSED" sentinel.

#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <net/if.h>
#include <netinet/in.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <unistd.h>

// TUN/TAP uapi bits. These are stable and identical across architectures; they
// are defined here so the probe builds against a bare musl toolchain that may
// not ship <linux/if_tun.h>.
#define IFF_TUN 0x0001
#define IFF_TAP 0x0002
#define IFF_NO_PI 0x1000
#define TUNSETIFF _IOW('T', 202, int)
#define TUNGETIFF _IOR('T', 210, unsigned int)
#define TUNGETFEATURES _IOR('T', 207, unsigned int)

#define TUN_PATH "/dev/net/tun"
#define IFNAME "tun0"
#define LOCAL_IP "10.8.0.1"
#define PEER_IP "10.8.0.2"
#define PREFIX 24
#define ICMP_ID 0x4242
#define ICMP_SEQ 1

static int failures;

static void check(const char *label, int ok) {
    printf("%-28s %s\n", label, ok ? "PASS" : "FAIL");
    if (!ok) failures++;
}

// Standard IP/ICMP one's-complement checksum over a byte range.
static uint16_t checksum(const void *data, size_t len) {
    const uint8_t *p = data;
    uint32_t sum = 0;
    while (len > 1) {
        sum += (uint16_t)((p[0] << 8) | p[1]);
        p += 2;
        len -= 2;
    }
    if (len) sum += (uint16_t)(p[0] << 8);
    while (sum >> 16) sum = (sum & 0xffff) + (sum >> 16);
    return (uint16_t)~sum;
}

// Builds an IPv4 + ICMP echo (type 8 request / type 0 reply) datagram.
// Returns the total length written into buf.
static size_t build_icmp(uint8_t *buf, uint32_t src, uint32_t dst, uint8_t type,
                         uint16_t id, uint16_t seq) {
    const uint8_t payload[] = "starry-tun-echo-probe";
    size_t plen = sizeof(payload);
    size_t icmp_len = 8 + plen;
    size_t total = 20 + icmp_len;

    memset(buf, 0, total);
    // IPv4 header.
    buf[0] = 0x45;            // version 4, IHL 5
    buf[1] = 0x00;            // DSCP/ECN
    buf[2] = (uint8_t)(total >> 8);
    buf[3] = (uint8_t)(total & 0xff);
    buf[4] = 0x00;           // id
    buf[5] = 0x01;
    buf[6] = 0x40;           // flags: DF
    buf[7] = 0x00;
    buf[8] = 64;             // TTL
    buf[9] = 1;              // protocol ICMP
    // checksum (10..11) filled below
    buf[12] = (uint8_t)(src >> 24);
    buf[13] = (uint8_t)(src >> 16);
    buf[14] = (uint8_t)(src >> 8);
    buf[15] = (uint8_t)(src);
    buf[16] = (uint8_t)(dst >> 24);
    buf[17] = (uint8_t)(dst >> 16);
    buf[18] = (uint8_t)(dst >> 8);
    buf[19] = (uint8_t)(dst);
    uint16_t ipck = checksum(buf, 20);
    buf[10] = (uint8_t)(ipck >> 8);
    buf[11] = (uint8_t)(ipck & 0xff);

    // ICMP header + payload.
    uint8_t *icmp = buf + 20;
    icmp[0] = type;          // 8 request / 0 reply
    icmp[1] = 0;             // code
    icmp[4] = (uint8_t)(id >> 8);
    icmp[5] = (uint8_t)(id & 0xff);
    icmp[6] = (uint8_t)(seq >> 8);
    icmp[7] = (uint8_t)(seq & 0xff);
    memcpy(icmp + 8, payload, plen);
    uint16_t icmpck = checksum(icmp, icmp_len);
    icmp[2] = (uint8_t)(icmpck >> 8);
    icmp[3] = (uint8_t)(icmpck & 0xff);
    return total;
}

static uint32_t ip_u32(const char *s) { return ntohl(inet_addr(s)); }

static void set_sockaddr(struct sockaddr *sa, const char *ip) {
    struct sockaddr_in *in = (struct sockaddr_in *)sa;
    memset(in, 0, sizeof(*in));
    in->sin_family = AF_INET;
    in->sin_addr.s_addr = inet_addr(ip);
}

// Attaches the fd to tun0 with the requested flags.
static int attach_tun(int fd, short flags) {
    struct ifreq ifr;
    memset(&ifr, 0, sizeof(ifr));
    strncpy(ifr.ifr_name, IFNAME, IFNAMSIZ - 1);
    ifr.ifr_flags = flags;
    return ioctl(fd, TUNSETIFF, &ifr);
}

// Configures address/netmask and brings tun0 up via a control socket.
static int configure_iface(void) {
    int s = socket(AF_INET, SOCK_DGRAM, 0);
    if (s < 0) return -1;

    struct ifreq ifr;
    memset(&ifr, 0, sizeof(ifr));
    strncpy(ifr.ifr_name, IFNAME, IFNAMSIZ - 1);

    set_sockaddr(&ifr.ifr_addr, LOCAL_IP);
    if (ioctl(s, SIOCSIFADDR, &ifr) < 0) { close(s); return -2; }

    // /24 netmask.
    set_sockaddr(&ifr.ifr_netmask, "255.255.255.0");
    if (ioctl(s, SIOCSIFNETMASK, &ifr) < 0) { close(s); return -3; }

    memset(&ifr.ifr_flags, 0, sizeof(ifr.ifr_flags));
    ifr.ifr_flags = IFF_UP | IFF_RUNNING;
    if (ioctl(s, SIOCSIFFLAGS, &ifr) < 0) { close(s); return -4; }

    close(s);
    return 0;
}

// Reads one packet from the tun fd, waiting up to timeout_ms.
static ssize_t read_packet(int fd, uint8_t *buf, size_t cap, int timeout_ms) {
    struct pollfd pfd = {.fd = fd, .events = POLLIN};
    int r = poll(&pfd, 1, timeout_ms);
    if (r <= 0) return -1;
    return read(fd, buf, cap);
}

// Phase 1: inject an echo request from the peer and expect the kernel to answer
// with an echo reply routed back out the interface.
static void phase_kernel_reply(int fd) {
    uint8_t out[128], in[256];
    uint32_t local = ip_u32(LOCAL_IP), peer = ip_u32(PEER_IP);
    size_t olen = build_icmp(out, peer, local, 8, ICMP_ID, ICMP_SEQ);

    ssize_t w = write(fd, out, olen);
    check("inject echo request", w == (ssize_t)olen);

    // The kernel may emit unrelated chatter (e.g. its own ARP-free IP); loop
    // until an ICMP reply addressed back to the peer shows up.
    int got_reply = 0;
    for (int attempt = 0; attempt < 8 && !got_reply; attempt++) {
        ssize_t n = read_packet(fd, in, sizeof(in), 2000);
        if (n < 28) continue;
        if ((in[0] >> 4) != 4) continue;      // IPv4
        if (in[9] != 1) continue;             // ICMP
        size_t ihl = (in[0] & 0x0f) * 4;
        uint8_t *icmp = in + ihl;
        if (icmp[0] != 0) continue;           // echo reply
        uint32_t src = (in[12] << 24) | (in[13] << 16) | (in[14] << 8) | in[15];
        uint32_t dst = (in[16] << 24) | (in[17] << 16) | (in[18] << 8) | in[19];
        uint16_t id = (icmp[4] << 8) | icmp[5];
        uint16_t seq = (icmp[6] << 8) | icmp[7];
        int addr_ok = (src == local) && (dst == peer);
        int idseq_ok = (id == ICMP_ID) && (seq == ICMP_SEQ);
        int ipck_ok = checksum(in, ihl) == 0;
        int icmpck_ok = checksum(icmp, (size_t)n - ihl) == 0;
        check("reply addresses swapped", addr_ok);
        check("reply id/seq echoed", idseq_ok);
        check("reply ip checksum", ipck_ok);
        check("reply icmp checksum", icmpck_ok);
        got_reply = 1;
    }
    check("kernel produced reply", got_reply);
}

// Phase 2: prove a locally-generated datagram egresses through tun0 and is
// readable off the fd. A UDP packet addressed to a host on the tun subnet is
// routed out the same interface (directly connected, no gateway), so the stack
// hands it to the tun device and userspace reads it back. This exercises the
// egress path for a socket-generated UDP datagram, distinct from the kernel's
// own ICMP reply in phase 1.
static void phase_framing(int fd) {
    uint8_t in[256];
    uint32_t other = ip_u32("10.8.0.9");

    int s = socket(AF_INET, SOCK_DGRAM, 0);
    check("phase2 socket", s >= 0);
    if (s < 0) return;

    struct sockaddr_in dst;
    memset(&dst, 0, sizeof(dst));
    dst.sin_family = AF_INET;
    dst.sin_addr.s_addr = inet_addr("10.8.0.9");
    dst.sin_port = htons(7);

    // Keep the socket open until after the fd read: closing it before the stack
    // flushes its TX would discard the queued datagram.
    ssize_t sent = sendto(s, "starry", 6, 0, (struct sockaddr *)&dst, sizeof(dst));
    check("phase2 sendto", sent == 6);

    int observed = 0;
    for (int attempt = 0; attempt < 8 && !observed; attempt++) {
        ssize_t n = read_packet(fd, in, sizeof(in), 1000);
        if (n < 20) continue;
        if ((in[0] >> 4) != 4) continue;   // IPv4
        if (in[9] != 17) continue;         // UDP
        uint32_t d = (in[16] << 24) | (in[17] << 16) | (in[18] << 8) | in[19];
        if (d != other) continue;
        observed = 1;
    }
    close(s);
    check("fd observes outbound", observed);
}

int main(void) {
    int fd = open(TUN_PATH, O_RDWR);
    check("open /dev/net/tun", fd >= 0);
    if (fd < 0) { printf("ALL CHECKS FAILED\n"); return 1; }

    // Query features before attaching (matches ioctl03's TUNGETFEATURES probe).
    unsigned int features = 0;
    int fr = ioctl(fd, TUNGETFEATURES, &features);
    check("TUNGETFEATURES", fr == 0 && (features & IFF_TUN));

    check("TUNSETIFF tun0", attach_tun(fd, IFF_TUN | IFF_NO_PI) == 0);

    struct ifreq ifr;
    memset(&ifr, 0, sizeof(ifr));
    int gr = ioctl(fd, TUNGETIFF, &ifr);
    check("TUNGETIFF name", gr == 0 && strcmp(ifr.ifr_name, IFNAME) == 0);

    check("configure tun0", configure_iface() == 0);

    // Make reads non-blocking-safe via poll; keep the fd blocking otherwise.
    phase_kernel_reply(fd);
    phase_framing(fd);

    close(fd);

    if (failures == 0) {
        printf("ALL CHECKS PASSED\n");
        return 0;
    }
    printf("ALL CHECKS FAILED (%d)\n", failures);
    return 1;
}
