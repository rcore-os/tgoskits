// TUN/TAP device ABI regression test.
//
// Exercises the /dev/net/tun character device interface against the Linux ABI:
// TUNSETIFF create/bind, TUNGETIFF round-trip, TUNGETFEATURES, second-fd EBUSY,
// TUNSETPERSIST device-level persist (full lifecycle: create/persist/close,
// reattach/close-without-clearing survives, explicit-clear removes), empty-name
// auto-allocation (tun%d/tap%d template), double-TUNSETIFF EEXIST,
// TAP SIOCGIFHWADDR (ARPHRD_ETHER + locally-administered MAC), create-close-recreate
// lifecycle, %d template name expansion, TUN PI-mode write (struct tun_pi + IPv4),
// TAP L2 Ethernet frame write (broadcast injection),
// TAP promiscuous ARP reply (inject an ARP request whose L2 destination MAC is a
// random unicast that is neither the interface MAC nor broadcast/multicast; only
// promiscuous mode accepts it, so the stack answers with an ARP reply that is
// read back - the observable flips red/green with set_promiscuous(true)),
// TUNSETIFF CAP_NET_ADMIN de-privilege EPERM (drop caps via setuid then TUNSETIFF),
// TUNSETIFF non-stranding after a failed bind (a failed TUNSETIFF must not leave
// the name pinned in Attached/EBUSY),
// TUN PI short-buffer read (kernel→user truncation sets TUN_PKT_STRIP per tun.c:2093),
// TAP PI short-buffer read (same path for EthernetDevice-backed TAP),
// SIOCSIFMTU/SIOCGIFMTU variable-MTU round-trip with dev_set_mtu bounds (TUN 68..65535,
// TAP 68..65521; out-of-range EINVAL, rejected set leaves the stored value intact),
// per-interface MTU datapath enforcement in both directions and both kinds: after
// LOWERING the MTU, an oversize write is dropped silently with write()==len (observed
// via /proc/net/dev rx_packets delta, checksum-independent) and an oversize egress
// frame never becomes readable, while a within-MTU inject is admitted and a raised MTU
// re-admits the previously-dropped egress frame; the TAP path pins the +14 Ethernet
// header accounting (frame ceiling = mtu + ETH_HLEN).
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include "test_framework.h"
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <net/if.h>
#include <netinet/in.h>
#include <poll.h>
#include <pthread.h>
#include <stdint.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef ARPHRD_ETHER
#define ARPHRD_ETHER 1
#endif

/* TUN/TAP ioctl commands - from Linux uapi/linux/if_tun.h, using int size. */
#ifndef TUNSETIFF
#define TUNSETIFF     0x400454cau
#endif
#ifndef TUNGETIFF
#define TUNGETIFF     0x800454d2u
#endif
#ifndef TUNSETPERSIST
#define TUNSETPERSIST 0x400454cbu
#endif
#ifndef TUNGETFEATURES
#define TUNGETFEATURES 0x800454cfu
#endif

/* TUN/TAP interface flags - from Linux uapi/linux/if_tun.h */
#ifndef IFF_TUN
#define IFF_TUN   0x0001
#endif
#ifndef IFF_TAP
#define IFF_TAP   0x0002
#endif
#ifndef IFF_NO_PI
#define IFF_NO_PI 0x1000
#endif

/* struct tun_pi constants */
#ifndef TUN_PKT_STRIP
#define TUN_PKT_STRIP 0x0001
#endif
/* sizeof(struct tun_pi) = 2 bytes flags + 2 bytes proto */
#define TUN_PI_LEN 4

/* Copy an interface name into an IFNAMSIZ buffer with a guaranteed trailing
 * NUL. strncpy(dst, src, IFNAMSIZ - 1) leaves the runner's -Werror Release
 * build tripping -Wstringop-truncation, so bound the length explicitly and
 * terminate ourselves - memcpy carries no truncation diagnostic. */
static void set_ifname(char *dst, const char *src) {
    size_t n = strnlen(src, IFNAMSIZ - 1);
    memcpy(dst, src, n);
    dst[n] = '\0';
}

static int open_tun(void) {
    int fd = open("/dev/net/tun", O_RDWR);
    CHECK(fd >= 0, "open /dev/net/tun");
    return fd;
}

static int tun_setiff(int fd, const char *name, short flags, char *out_name) {
    struct ifreq ifr;
    memset(&ifr, 0, sizeof(ifr));
    if (name && *name)
        set_ifname(ifr.ifr_name, name);
    ifr.ifr_flags = flags;
    int r = ioctl(fd, TUNSETIFF, &ifr);
    if (r == 0 && out_name)
        set_ifname(out_name, ifr.ifr_name);
    return r;
}

/* Reads the rx_packets counter for `ifname` from /proc/net/dev, or -1 if the
 * interface line is not found. The line is `  <name>: <rx_bytes> <rx_packets>
 * ...`; rx_packets is the second whitespace field after the `:`. This is the
 * checksum-independent write-side observation: TunShared::push_rx drops an
 * over-MTU inject before it enters the RX queue, so a dropped frame never
 * reaches TunDevice::recv and never increments rx_packets, whereas an accepted
 * within-MTU frame is drained and counted - and this happens at queue-drain
 * time, upstream of the IP-layer checksum verification that would defeat a
 * bound-socket delivery observation. */
static long read_rx_packets(const char *ifname) {
    FILE *f = fopen("/proc/net/dev", "r");
    if (!f)
        return -1;
    char line[512];
    long result = -1;
    while (fgets(line, sizeof(line), f)) {
        char *colon = strchr(line, ':');
        if (!colon)
            continue;
        /* The name field is right-justified with leading spaces; trim them. */
        char *name = line;
        while (*name == ' ' || *name == '\t')
            name++;
        size_t namelen = (size_t)(colon - name);
        if (namelen != strlen(ifname) || strncmp(name, ifname, namelen) != 0)
            continue;
        unsigned long long rx_bytes, rx_packets;
        if (sscanf(colon + 1, "%llu %llu", &rx_bytes, &rx_packets) == 2)
            result = (long)rx_packets;
        break;
    }
    fclose(f);
    return result;
}

// TUNSETIFF creates a TUN device; TUNGETIFF echoes the allocated name and flags.
static void test_create_and_getiff(void) {
    int fd = open_tun();
    if (fd < 0) return;

    CHECK_RET(tun_setiff(fd, "tstun0", IFF_TUN | IFF_NO_PI, NULL), 0,
              "TUNSETIFF IFF_TUN|IFF_NO_PI on tstun0 succeeds");

    struct ifreq ifr;
    memset(&ifr, 0, sizeof(ifr));
    CHECK_RET(ioctl(fd, TUNGETIFF, &ifr), 0, "TUNGETIFF on bound fd succeeds");
    CHECK(strncmp(ifr.ifr_name, "tstun0", IFNAMSIZ) == 0,
          "TUNGETIFF returns correct name tstun0");
    CHECK((ifr.ifr_flags & (IFF_TUN | IFF_NO_PI)) == (IFF_TUN | IFF_NO_PI),
          "TUNGETIFF returns IFF_TUN|IFF_NO_PI flags");

    close(fd);
}

// A second TUNSETIFF on an already-attached fd returns EEXIST (Linux tun_chr_ioctl).
static void test_double_setiff_eexist(void) {
    int fd = open_tun();
    if (fd < 0) return;

    CHECK_RET(tun_setiff(fd, "tstun1", IFF_TUN | IFF_NO_PI, NULL), 0,
              "first TUNSETIFF on tstun1 succeeds");
    CHECK_ERR(tun_setiff(fd, "tstun1", IFF_TUN | IFF_NO_PI, NULL), EEXIST,
              "second TUNSETIFF on already-bound fd returns EEXIST");

    close(fd);
}

// Binding a second fd to a non-multi-queue TUN returns EBUSY.
static void test_second_fd_ebusy(void) {
    int fd1 = open_tun();
    int fd2 = open_tun();
    if (fd1 < 0 || fd2 < 0) { close(fd1); close(fd2); return; }

    CHECK_RET(tun_setiff(fd1, "tstun2", IFF_TUN | IFF_NO_PI, NULL), 0,
              "first fd binds tstun2");
    CHECK_ERR(tun_setiff(fd2, "tstun2", IFF_TUN | IFF_NO_PI, NULL), EBUSY,
              "second fd on same non-multi-queue TUN returns EBUSY");

    close(fd1);
    close(fd2);
}

// An empty ifr_name triggers auto-allocation ("tun%d" template, Linux idiom).
static void test_empty_name_autoalloc(void) {
    int fd = open_tun();
    if (fd < 0) return;

    char name[IFNAMSIZ] = {0};
    CHECK_RET(tun_setiff(fd, "", IFF_TUN | IFF_NO_PI, name), 0,
              "TUNSETIFF with empty name succeeds (auto-alloc)");
    CHECK(strlen(name) > 0 && strncmp(name, "tun", 3) == 0,
          "auto-allocated name starts with 'tun'");

    close(fd);
}

// TUNSETPERSIST sets IFF_PERSIST at device level (Linux tun->flags, not per-fd).
// Lifecycle: create→persist→close-without-clearing (device survives)→
//            reattach→close-without-clearing (device survives again)→
//            reattach→explicit-clear→close (device removed).
static void test_persist_device_level(void) {
    int fd1 = open_tun();
    if (fd1 < 0) return;

    CHECK_RET(tun_setiff(fd1, "tstun3", IFF_TUN | IFF_NO_PI, NULL), 0,
              "bind tstun3");
    CHECK_RET(ioctl(fd1, TUNSETPERSIST, 1L), 0,
              "TUNSETPERSIST enable on tstun3");
    close(fd1);

    // Re-attach without changing persist; closing WITHOUT explicit clear must
    // leave the device alive (IFF_PERSIST is device-level, not per-fd).
    int fd2 = open_tun();
    if (fd2 < 0) return;
    CHECK_RET(tun_setiff(fd2, "tstun3", IFF_TUN | IFF_NO_PI, NULL), 0,
              "re-attach tstun3 after original fd closed (step 2)");
    close(fd2);

    // Device must still exist after fd2 closed without clearing persist.
    int fd3 = open_tun();
    if (fd3 < 0) return;
    CHECK_RET(tun_setiff(fd3, "tstun3", IFF_TUN | IFF_NO_PI, NULL), 0,
              "re-attach tstun3 after close-without-clearing persists (survives)");

    // Only an explicit TUNSETPERSIST(0) removes the device on the next close.
    CHECK_RET(ioctl(fd3, TUNSETPERSIST, 0L), 0, "TUNSETPERSIST disable on tstun3");
    close(fd3);
}

// TUNGETFEATURES reports the flag bits this driver supports.
static void test_get_features(void) {
    int fd = open_tun();
    if (fd < 0) return;

    unsigned int features = 0;
    CHECK_RET(ioctl(fd, TUNGETFEATURES, &features), 0,
              "TUNGETFEATURES on unattached fd succeeds");
    CHECK(features & IFF_TUN, "TUNGETFEATURES includes IFF_TUN");
    CHECK(features & IFF_TAP, "TUNGETFEATURES includes IFF_TAP");
    CHECK(features & IFF_NO_PI, "TUNGETFEATURES includes IFF_NO_PI");

    close(fd);
}

// TUNSETIFF rejects flag bits outside {IFF_TUN, IFF_TAP, IFF_NO_PI}.
static void test_unsupported_flags_einval(void) {
    int fd = open_tun();
    if (fd < 0) return;

    // IFF_VNET_HDR (0x4000) is not supported; must return EINVAL.
    CHECK_ERR(tun_setiff(fd, "tstun4", IFF_TUN | 0x4000, NULL), EINVAL,
              "TUNSETIFF with unsupported flag bit returns EINVAL");

    close(fd);
}

// TAP create: SIOCGIFHWADDR must return ARPHRD_ETHER with locally-administered MAC.
// Linux ether_setup() sets sa_family=ARPHRD_ETHER and assigns a random
// locally-administered unicast MAC (bit 1 of byte 0 is set, bit 0 is clear).
static void test_tap_create_hwaddr(void) {
    int fd = open_tun();
    if (fd < 0) return;

    CHECK_RET(tun_setiff(fd, "tstap0", IFF_TAP | IFF_NO_PI, NULL), 0,
              "TUNSETIFF IFF_TAP|IFF_NO_PI on tstap0 succeeds");

    int sock = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(sock >= 0, "open AF_INET socket for SIOCGIFHWADDR");
    if (sock >= 0) {
        struct ifreq ifr;
        memset(&ifr, 0, sizeof(ifr));
        set_ifname(ifr.ifr_name, "tstap0");
        CHECK_RET(ioctl(sock, SIOCGIFHWADDR, &ifr), 0,
                  "SIOCGIFHWADDR on tstap0 succeeds");
        CHECK((unsigned short)ifr.ifr_hwaddr.sa_family == ARPHRD_ETHER,
              "TAP sa_family is ARPHRD_ETHER");
        // bit 1 set = locally administered; bit 0 clear = unicast
        CHECK((ifr.ifr_hwaddr.sa_data[0] & 0x03) == 0x02,
              "TAP MAC is locally-administered unicast");
        close(sock);
    }
    close(fd);
}

// SIOCSIFMTU sets a per-interface MTU; SIOCGIFMTU reads it back. Linux gates the
// value against dev->min_mtu/max_mtu (tun_net_init: 68 .. 65535 for a header-less
// TUN, 68 .. 65521 for an Ethernet TAP). Old behaviour rejected any value other
// than the fixed 1500 with EINVAL, so the "set 1420, read back 1420" assertions
// below fail red; with the variable-MTU support they pass green.
static void test_set_get_mtu(void) {
    int fd = open_tun();
    if (fd < 0) return;

    CHECK_RET(tun_setiff(fd, "tstmtu0", IFF_TUN | IFF_NO_PI, NULL), 0,
              "TUNSETIFF IFF_TUN|IFF_NO_PI on tstmtu0 succeeds");

    int sock = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(sock >= 0, "open AF_INET socket for SIOCSIFMTU/SIOCGIFMTU");
    if (sock >= 0) {
        // Set a legal non-default MTU and read it back: the value must round-trip
        // (the old fixed-1500 code returned EINVAL here).
        struct ifreq ifr;
        memset(&ifr, 0, sizeof(ifr));
        set_ifname(ifr.ifr_name, "tstmtu0");
        ifr.ifr_mtu = 1420;
        CHECK_RET(ioctl(sock, SIOCSIFMTU, &ifr), 0,
                  "SIOCSIFMTU 1420 on tstmtu0 succeeds");

        memset(&ifr, 0, sizeof(ifr));
        set_ifname(ifr.ifr_name, "tstmtu0");
        CHECK_RET(ioctl(sock, SIOCGIFMTU, &ifr), 0,
                  "SIOCGIFMTU on tstmtu0 succeeds");
        CHECK(ifr.ifr_mtu == 1420, "SIOCGIFMTU reads back the value just set (1420)");

        // A second distinct value also round-trips (proves it is stored, not
        // pinned to any single constant).
        memset(&ifr, 0, sizeof(ifr));
        set_ifname(ifr.ifr_name, "tstmtu0");
        ifr.ifr_mtu = 1280;
        CHECK_RET(ioctl(sock, SIOCSIFMTU, &ifr), 0,
                  "SIOCSIFMTU 1280 on tstmtu0 succeeds");
        memset(&ifr, 0, sizeof(ifr));
        set_ifname(ifr.ifr_name, "tstmtu0");
        CHECK_RET(ioctl(sock, SIOCGIFMTU, &ifr), 0,
                  "SIOCGIFMTU on tstmtu0 succeeds (second value)");
        CHECK(ifr.ifr_mtu == 1280, "SIOCGIFMTU reads back the second value (1280)");

        // Out-of-range values are rejected with EINVAL, matching dev_set_mtu's
        // min_mtu/max_mtu bounds check: below MIN_MTU (68) and above a TUN's
        // MAX_MTU (65535).
        memset(&ifr, 0, sizeof(ifr));
        set_ifname(ifr.ifr_name, "tstmtu0");
        ifr.ifr_mtu = 67; /* MIN_MTU - 1 */
        CHECK_ERR(ioctl(sock, SIOCSIFMTU, &ifr), EINVAL,
                  "SIOCSIFMTU below MIN_MTU (67) is rejected EINVAL");

        memset(&ifr, 0, sizeof(ifr));
        set_ifname(ifr.ifr_name, "tstmtu0");
        ifr.ifr_mtu = 65536; /* MAX_MTU + 1 for a TUN */
        CHECK_ERR(ioctl(sock, SIOCSIFMTU, &ifr), EINVAL,
                  "SIOCSIFMTU above MAX_MTU (65536) is rejected EINVAL");

        // A rejected out-of-range set must not have clobbered the last accepted
        // value: MTU still reads back 1280.
        memset(&ifr, 0, sizeof(ifr));
        set_ifname(ifr.ifr_name, "tstmtu0");
        CHECK_RET(ioctl(sock, SIOCGIFMTU, &ifr), 0,
                  "SIOCGIFMTU after rejected sets still succeeds");
        CHECK(ifr.ifr_mtu == 1280,
              "rejected out-of-range SIOCSIFMTU left the stored MTU unchanged (1280)");

        close(sock);
    }
    close(fd);
}

// A TAP's upper MTU bound is MAX_MTU - ETH_HLEN = 65521 (Linux ether_setup sets
// hard_header_len = ETH_HLEN). 65521 must be accepted and 65522 rejected, unlike
// a header-less TUN which accepts up to 65535.
static void test_tap_mtu_upper_bound(void) {
    int fd = open_tun();
    if (fd < 0) return;

    CHECK_RET(tun_setiff(fd, "tstmtap0", IFF_TAP | IFF_NO_PI, NULL), 0,
              "TUNSETIFF IFF_TAP|IFF_NO_PI on tstmtap0 succeeds");

    int sock = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(sock >= 0, "open AF_INET socket for TAP MTU bound test");
    if (sock >= 0) {
        struct ifreq ifr;
        memset(&ifr, 0, sizeof(ifr));
        set_ifname(ifr.ifr_name, "tstmtap0");
        ifr.ifr_mtu = 65521; /* MAX_MTU - ETH_HLEN */
        CHECK_RET(ioctl(sock, SIOCSIFMTU, &ifr), 0,
                  "SIOCSIFMTU 65521 (TAP max) succeeds");

        memset(&ifr, 0, sizeof(ifr));
        set_ifname(ifr.ifr_name, "tstmtap0");
        ifr.ifr_mtu = 65522; /* one past the TAP max */
        CHECK_ERR(ioctl(sock, SIOCSIFMTU, &ifr), EINVAL,
                  "SIOCSIFMTU 65522 (past TAP max) is rejected EINVAL");

        close(sock);
    }
    close(fd);
}

// Non-persistent TUN must vanish when its last fd is closed; creating an
// interface with the same name immediately after must succeed (Linux tun.c
// unregisters the netdev on last detach when IFF_PERSIST is clear).
static void test_create_close_recreate_tun(void) {
    int fd = open_tun();
    if (fd < 0) return;

    CHECK_RET(tun_setiff(fd, "tstun6", IFF_TUN | IFF_NO_PI, NULL), 0,
              "create tstun6 (non-persistent)");
    close(fd);

    fd = open_tun();
    if (fd < 0) return;
    CHECK_RET(tun_setiff(fd, "tstun6", IFF_TUN | IFF_NO_PI, NULL), 0,
              "recreate tstun6 after close succeeds (device was removed)");
    close(fd);
}

// Same lifecycle test for TAP.
static void test_create_close_recreate_tap(void) {
    int fd = open_tun();
    if (fd < 0) return;

    CHECK_RET(tun_setiff(fd, "tstap1", IFF_TAP | IFF_NO_PI, NULL), 0,
              "create tstap1 (non-persistent)");
    close(fd);

    fd = open_tun();
    if (fd < 0) return;
    CHECK_RET(tun_setiff(fd, "tstap1", IFF_TAP | IFF_NO_PI, NULL), 0,
              "recreate tstap1 after close succeeds (device was removed)");
    close(fd);
}

// Linux supports "tun%d"/"tap%d" template names: %d is expanded to the first
// available decimal suffix.  The returned name must start with the prefix and
// must not contain a literal percent sign.
static void test_template_name_alloc(void) {
    int fd = open_tun();
    if (fd < 0) return;

    char name[IFNAMSIZ] = {0};
    CHECK_RET(tun_setiff(fd, "tun%d", IFF_TUN | IFF_NO_PI, name), 0,
              "TUNSETIFF with 'tun%%d' template succeeds");
    CHECK(strncmp(name, "tun", 3) == 0 && strchr(name, '%') == NULL,
          "template 'tun%%d' expands to tun<N> with no literal %%");
    close(fd);

    fd = open_tun();
    if (fd < 0) return;
    memset(name, 0, sizeof(name));
    CHECK_RET(tun_setiff(fd, "tap%d", IFF_TAP | IFF_NO_PI, name), 0,
              "TUNSETIFF with 'tap%%d' template succeeds");
    CHECK(strncmp(name, "tap", 3) == 0 && strchr(name, '%') == NULL,
          "template 'tap%%d' expands to tap<N> with no literal %%");
    close(fd);
}

// TUN created without IFF_NO_PI (PI mode active): TUNGETIFF must echo the
// absence of IFF_NO_PI so user space knows PI headers are in effect.
// Then verify the PI write framing: user→kernel direction prepends struct tun_pi
// (flags=0, proto=ETH_P_IP) to each IPv4 packet written to the fd.
static void test_tun_pi_write(void) {
    int fd = open_tun();
    if (fd < 0) return;

    CHECK_RET(tun_setiff(fd, "tstun7", IFF_TUN, NULL), 0,
              "TUNSETIFF IFF_TUN without IFF_NO_PI (PI mode) succeeds");

    struct ifreq ifr;
    memset(&ifr, 0, sizeof(ifr));
    CHECK_RET(ioctl(fd, TUNGETIFF, &ifr), 0, "TUNGETIFF on PI-mode fd succeeds");
    CHECK(ifr.ifr_flags & IFF_TUN, "PI-mode fd reports IFF_TUN");
    CHECK(!(ifr.ifr_flags & IFF_NO_PI), "PI-mode fd does not report IFF_NO_PI");

    // struct tun_pi (4 bytes): flags=0, proto=ETH_P_IP (0x0800, big-endian).
    // Followed by a minimal IPv4 packet (20-byte header, 8-byte payload).
    // Checksums are deliberately wrong here; the kernel accepts raw inject.
    unsigned char pkt[4 + 28] = {
        /* tun_pi */    0x00, 0x00, 0x08, 0x00,
        /* IPv4 hdr */  0x45, 0x00, 0x00, 0x1c,  /* ver/IHL, TOS, total len */
                        0x00, 0x01, 0x40, 0x00,  /* ID, flags, frag */
                        0x40, 0x11, 0x00, 0x00,  /* TTL, proto=UDP, chksum */
                        0xc0, 0xa8, 0x64, 0x02,  /* src 192.168.100.2 */
                        0xc0, 0xa8, 0x64, 0x01,  /* dst 192.168.100.1 */
        /* UDP hdr */   0x00, 0x50, 0x00, 0x51, 0x00, 0x0c, 0x00, 0x00,
    };
    // write() to TUN fd injects packet into the kernel; return value = bytes written.
    ssize_t n = write(fd, pkt, sizeof(pkt));
    CHECK(n == (ssize_t)sizeof(pkt), "PI-mode write(struct tun_pi + IPv4) returns full length");

    close(fd);
}

// TAP L2 write: user injects an Ethernet frame into the kernel via write(2).
// Linux tun_get_user() accepts the frame without filtering when TAP is in
// promiscuous mode (which create_tap enables).
static void test_tap_frame_write(void) {
    int fd = open_tun();
    if (fd < 0) return;

    CHECK_RET(tun_setiff(fd, "tstap2", IFF_TAP | IFF_NO_PI, NULL), 0,
              "TUNSETIFF IFF_TAP|IFF_NO_PI on tstap2 succeeds");

    // ARP request (60 bytes) broadcast frame.
    unsigned char frame[60] = {
        /* dst MAC = broadcast */   0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        /* src MAC = local-admin */ 0x02, 0x00, 0xde, 0xad, 0xbe, 0xef,
        /* ethertype ARP */         0x08, 0x06,
        /* ARP payload */           0x00, 0x01, 0x08, 0x00, 0x06, 0x04,
                                    0x00, 0x01, 0x02, 0x00, 0xde, 0xad, 0xbe, 0xef,
                                    0xc0, 0xa8, 0x64, 0x02,
                                    0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                                    0xc0, 0xa8, 0x64, 0x01,
        /* padding */
    };
    ssize_t n = write(fd, frame, sizeof(frame));
    CHECK(n == (ssize_t)sizeof(frame), "TAP IFF_NO_PI write(Ethernet frame) returns full length");

    close(fd);
}

// TAP promiscuous-mode regression (old-red / new-green).
//
// This is the valid regression for the `create_tap` `set_promiscuous(true)` fix
// (device/tun.rs). A bare `write()==len` assertion is NOT valid: write() always
// reports the full length once the frame is queued (tun.rs push()/write_at),
// regardless of whether the frame is later ACCEPTED or dropped at smoltcp's MAC
// filter (ethernet.rs handle_frame). So a write-length check passes on pre-fix
// code too and proves nothing.
//
// Instead we inject an ARP *request* whose L2 Ethernet destination MAC is a
// RANDOM UNICAST address that is neither the interface's own MAC nor
// broadcast/multicast, targeting the TAP's own IP, and then read back the reply:
//
//   handle_frame() (ethernet.rs) drops any frame when
//       !promiscuous && dst != broadcast && dst != 00:00:00:00:00:00 && dst != own_mac
//   Our L2 dst is a random unicast, so:
//     - pre-fix (no set_promiscuous): the frame is dropped at the MAC filter,
//       process_arp() never runs, no ARP reply is emitted, the fd never becomes
//       readable -> poll() times out -> RED.
//     - post-fix (set_promiscuous(true)): the filter is bypassed, process_arp()
//       runs, matches target_protocol_addr against the TAP IP, and emits an ARP
//       reply into the tx_queue -> the fd becomes readable and read() returns a
//       42-byte ARP reply -> GREEN.
//
// This mirrors Linux tun_get_user()/netif_rx injecting TAP frames without a
// destination-MAC filter (tun.c). Note the ARP-layer target hardware address is
// the conventional all-zeros (EMPTY_MAC), so process_arp's own is_unicast_mac
// guard passes; only the L2 promiscuous filter gates acceptance, which is
// precisely what this test isolates.
//
// The TAP MAC is interface-id-derived (service.rs tap_mac: 02:00:<id BE bytes>)
// and thus not known a priori, so we read it via SIOCGIFHWADDR and deliberately
// pick an L2 dst that differs from it (flip a byte), guaranteeing a non-own,
// non-broadcast, non-multicast unicast destination.
static void test_tap_promiscuous_arp_reply(void) {
    int fd = open_tun();
    if (fd < 0) return;

    char name[IFNAMSIZ] = {};
    // IFF_NO_PI: read()/write() carry the bare Ethernet frame with no tun_pi
    // header, so the readback compares directly against ARP-reply bytes.
    CHECK_RET(tun_setiff(fd, "tstap3", IFF_TAP | IFF_NO_PI, name), 0,
              "TUNSETIFF IFF_TAP|IFF_NO_PI on tstap3 succeeds");

    int csock = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(csock >= 0, "control socket for TAP promiscuous test");
    if (csock < 0) { close(fd); return; }

    // Learn the interface MAC so we can address the frame to a DIFFERENT unicast.
    struct ifreq ifr_hw = {};
    set_ifname(ifr_hw.ifr_name, name);
    CHECK_RET(ioctl(csock, SIOCGIFHWADDR, &ifr_hw), 0,
              "SIOCGIFHWADDR reads tstap3 MAC");
    unsigned char if_mac[6];
    memcpy(if_mac, ifr_hw.ifr_hwaddr.sa_data, 6);

    // Assign 10.68.0.1/24 to the TAP and bring it UP so the stack owns the IP
    // and will answer ARP for it. (Same SIOCSIFADDR/SIOCSIFFLAGS path the PI
    // short-read tests use; confirms TAP interfaces accept a runtime address.)
    struct ifreq ifr_a = {};
    set_ifname(ifr_a.ifr_name, name);
    struct sockaddr_in *sa = (struct sockaddr_in *)&ifr_a.ifr_addr;
    sa->sin_family = AF_INET;
    sa->sin_addr.s_addr = htonl(0x0A440001u); /* 10.68.0.1 */
    CHECK_RET(ioctl(csock, SIOCSIFADDR, &ifr_a), 0,
              "SIOCSIFADDR 10.68.0.1/24 on tstap3");

    struct ifreq ifr_f = {};
    set_ifname(ifr_f.ifr_name, name);
    ifr_f.ifr_flags = IFF_UP;
    CHECK_RET(ioctl(csock, SIOCSIFFLAGS, &ifr_f), 0,
              "SIOCSIFFLAGS IFF_UP on tstap3");

    // Peer that "asks" for the TAP's IP. Any valid unicast peer MAC/IP works;
    // it becomes the ARP reply's destination.
    const unsigned char peer_mac[6] = {0x02, 0x00, 0xca, 0xfe, 0x00, 0x02};
    /* peer IP 10.68.0.2 (unicast, same subnet) */
    const unsigned char peer_ip[4] = {0x0a, 0x44, 0x00, 0x02};
    /* TAP IP 10.68.0.1, the ARP target we resolve */
    const unsigned char tap_ip[4] = {0x0a, 0x44, 0x00, 0x01};

    // L2 destination MAC = a random unicast distinct from the interface MAC.
    // Copy the interface MAC and flip a byte: the result stays a locally-
    // administered unicast (LSB of byte 0 = 0) but is provably not our own MAC,
    // not broadcast (ff:ff:...), and not multicast (bit 0 of byte 0 = 0). Only
    // promiscuous mode lets it through the L2 filter.
    unsigned char l2_dst[6];
    memcpy(l2_dst, if_mac, 6);
    l2_dst[5] ^= 0xaa; /* differ from own MAC while preserving unicast bits */

    // 42-byte ARP request: 14-byte Ethernet header + 28-byte ARP payload.
    unsigned char frame[42];
    memset(frame, 0, sizeof(frame));
    memcpy(&frame[0], l2_dst, 6);          /* Ethernet dst: random unicast */
    memcpy(&frame[6], peer_mac, 6);        /* Ethernet src: peer */
    frame[12] = 0x08; frame[13] = 0x06;    /* EtherType = ARP */
    frame[14] = 0x00; frame[15] = 0x01;    /* HTYPE = Ethernet */
    frame[16] = 0x08; frame[17] = 0x00;    /* PTYPE = IPv4 */
    frame[18] = 0x06;                      /* HLEN = 6 */
    frame[19] = 0x04;                      /* PLEN = 4 */
    frame[20] = 0x00; frame[21] = 0x01;    /* OPER = request */
    memcpy(&frame[22], peer_mac, 6);       /* sender hardware addr = peer */
    memcpy(&frame[28], peer_ip, 4);        /* sender protocol addr = 10.68.0.2 */
    /* target hardware addr = 00:00:00:00:00:00 (unknown; conventional for a
     * request) -> already zeroed by memset */
    memcpy(&frame[38], tap_ip, 4);         /* target protocol addr = TAP IP */

    ssize_t n = write(fd, frame, sizeof(frame));
    CHECK(n == (ssize_t)sizeof(frame),
          "inject ARP request (random-unicast L2 dst) into tstap3");

    // Bounded wait: pre-fix the frame is filtered and no reply is ever produced,
    // so poll() must time out (fast RED) rather than hang.
    struct pollfd pfd = { .fd = fd, .events = POLLIN };
    fflush(stdout);
    int pr = poll(&pfd, 1, 2000);
    CHECK(pr == 1 && (pfd.revents & POLLIN),
          "tstap3 becomes readable = ARP reply emitted (promiscuous accept)");
    if (pr != 1 || !(pfd.revents & POLLIN)) {
        // Pre-fix path lands here: make the promiscuous regression explicit.
        CHECK(0, "no ARP reply within 2s: frame dropped at MAC filter "
                 "(set_promiscuous(true) reverted?)");
        close(csock);
        close(fd);
        return;
    }

    unsigned char reply[64];
    memset(reply, 0, sizeof(reply));
    ssize_t rn = read(fd, reply, sizeof(reply));
    CHECK(rn >= 42, "read back a full-size ARP reply frame from tstap3");
    if (rn >= 42) {
        // EtherType must be ARP and the opcode must be REPLY (2): this proves the
        // stack actually processed the request rather than echoing our input.
        CHECK(reply[12] == 0x08 && reply[13] == 0x06,
              "reply EtherType is ARP");
        CHECK(reply[20] == 0x00 && reply[21] == 0x02,
              "reply ARP opcode is REPLY (stack answered our request)");
        // The reply resolves the TAP IP to the interface's own MAC.
        CHECK(memcmp(&reply[28], tap_ip, 4) == 0,
              "reply sender protocol addr is the TAP IP");
        CHECK(memcmp(&reply[22], if_mac, 6) == 0,
              "reply sender hardware addr is the TAP MAC");
        // And it is unicast back to the asking peer.
        CHECK(memcmp(&reply[0], peer_mac, 6) == 0,
              "reply is unicast to the requesting peer MAC");
    }

    close(csock);
    close(fd);
}

/* rtnetlink RTM_GETLINK / RTM_GETADDR broadcast attributes for TAP.
 *
 * Linux ether_setup() (called from tun.c for the IFF_TAP case) makes a tap a
 * broadcast Ethernet device: ARPHRD_ETHER link type, IFF_BROADCAST set, and a
 * link-layer broadcast address of ff:ff:ff:ff:ff:ff. iproute2 reads those via
 * an RTM_GETLINK dump (IFLA_BROADCAST attribute + ifi_type + ifi_flags) and the
 * per-address IPv4 broadcast via RTM_GETADDR (IFA_BROADCAST). The netlink
 * responder must expose the same for a StarryOS TAP.
 *
 * Local netlink definitions with a `_nl` suffix: musl's linux uapi netlink
 * headers are not guaranteed present in the runner sysroot, so mirror the
 * subset used here (as the other netlink regression tests in this suite do). */
#ifndef AF_NETLINK
#define AF_NETLINK 16
#endif
#ifndef AF_PACKET
#define AF_PACKET 17
#endif
#define NETLINK_ROUTE_NL 0
#define NLM_F_REQUEST_NL 0x01
#define NLM_F_DUMP_NL 0x300
#define NLMSG_DONE_NL 3
#define RTM_GETLINK_NL 18
#define RTM_NEWLINK_NL 16
#define RTM_GETADDR_NL 22
#define RTM_NEWADDR_NL 20
#define IFLA_ADDRESS_NL 1
#define IFLA_BROADCAST_NL 2
#define IFLA_IFNAME_NL 3
#define IFA_ADDRESS_NL 1
#define IFA_LABEL_NL 3
#define IFA_BROADCAST_NL 4
#define IFF_BROADCAST_NL 0x0002u

#define NLMSG_ALIGNTO_NL 4U
#define NLMSG_ALIGN_NL(len) (((len) + NLMSG_ALIGNTO_NL - 1) & ~(NLMSG_ALIGNTO_NL - 1))
#define NLMSG_HDRLEN_NL ((int)NLMSG_ALIGN_NL(sizeof(struct nlmsghdr_nl)))
#define NLMSG_DATA_NL(nlh) ((void *)((char *)(nlh) + NLMSG_HDRLEN_NL))
#define NLMSG_NEXT_NL(nlh, len)                                                 \
    ((len) -= NLMSG_ALIGN_NL((nlh)->nlmsg_len),                                 \
     (struct nlmsghdr_nl *)((char *)(nlh) + NLMSG_ALIGN_NL((nlh)->nlmsg_len)))
#define NLMSG_OK_NL(nlh, len)                                                   \
    ((len) >= (int)sizeof(struct nlmsghdr_nl) &&                                \
     (nlh)->nlmsg_len >= sizeof(struct nlmsghdr_nl) &&                          \
     (nlh)->nlmsg_len <= (unsigned int)(len))

#define RTA_ALIGNTO_NL 4U
#define RTA_ALIGN_NL(len) (((len) + RTA_ALIGNTO_NL - 1) & ~(RTA_ALIGNTO_NL - 1))
#define RTA_LENGTH_NL(len) (RTA_ALIGN_NL(sizeof(struct rtattr_nl)) + (len))
#define RTA_DATA_NL(rta) ((void *)((char *)(rta) + RTA_LENGTH_NL(0)))
#define RTA_PAYLOAD_NL(rta) ((int)((rta)->rta_len) - RTA_LENGTH_NL(0))
#define RTA_NEXT_NL(rta, attrlen)                                              \
    ((attrlen) -= RTA_ALIGN_NL((rta)->rta_len),                                \
     (struct rtattr_nl *)((char *)(rta) + RTA_ALIGN_NL((rta)->rta_len)))
#define RTA_OK_NL(rta, len)                                                    \
    ((len) >= (int)sizeof(struct rtattr_nl) &&                                 \
     (rta)->rta_len >= sizeof(struct rtattr_nl) &&                             \
     (rta)->rta_len <= (unsigned int)(len))

struct sockaddr_nl_nl {
    unsigned short nl_family;
    unsigned short nl_pad;
    unsigned int nl_pid;
    unsigned int nl_groups;
};
struct nlmsghdr_nl {
    unsigned int nlmsg_len;
    unsigned short nlmsg_type;
    unsigned short nlmsg_flags;
    unsigned int nlmsg_seq;
    unsigned int nlmsg_pid;
};
struct rtgenmsg_nl {
    unsigned char rtgen_family;
};
struct ifinfomsg_nl {
    unsigned char ifi_family;
    unsigned char __ifi_pad;
    unsigned short ifi_type;
    int ifi_index;
    unsigned int ifi_flags;
    unsigned int ifi_change;
};
struct ifaddrmsg_nl {
    unsigned char ifa_family;
    unsigned char ifa_prefixlen;
    unsigned char ifa_flags;
    unsigned char ifa_scope;
    unsigned int ifa_index;
};
struct rtattr_nl {
    unsigned short rta_len;
    unsigned short rta_type;
};

static int nl_send_dump(int fd, unsigned short type, unsigned char family, unsigned int seq) {
    struct {
        struct nlmsghdr_nl nh;
        struct rtgenmsg_nl gen;
    } req;
    memset(&req, 0, sizeof(req));
    req.nh.nlmsg_len = sizeof(req);
    req.nh.nlmsg_type = type;
    req.nh.nlmsg_flags = NLM_F_REQUEST_NL | NLM_F_DUMP_NL;
    req.nh.nlmsg_seq = seq;
    req.gen.rtgen_family = family;
    return write(fd, &req, sizeof(req)) == (ssize_t)sizeof(req);
}

// Dump RTM_GETLINK and assert the TAP named `want` carries ARPHRD_ETHER,
// IFF_BROADCAST, and an IFLA_BROADCAST of ff:ff:ff:ff:ff:ff.
static void nl_check_tap_getlink(int fd, const char *want) {
    char buf[8192];
    int found = 0, is_ether = 0, has_bcast_flag = 0, bcast_all_ff = 0, done = 0;

    while (!done) {
        ssize_t n = read(fd, buf, sizeof(buf));
        if (n < 0) {
            CHECK(0, "read RTM_GETLINK dump for TAP");
            return;
        }
        int rem = (int)n;
        for (struct nlmsghdr_nl *nh = (struct nlmsghdr_nl *)buf; NLMSG_OK_NL(nh, rem);
             nh = NLMSG_NEXT_NL(nh, rem)) {
            if (nh->nlmsg_type == NLMSG_DONE_NL) { done = 1; break; }
            if (nh->nlmsg_type != RTM_NEWLINK_NL) continue;
            struct ifinfomsg_nl *ifi = NLMSG_DATA_NL(nh);
            const char *this_name = NULL;
            unsigned char bcast[6] = {0};
            int saw_bcast_attr = 0;
            int len = (int)nh->nlmsg_len - NLMSG_HDRLEN_NL - (int)sizeof(*ifi);
            for (struct rtattr_nl *a = (struct rtattr_nl *)((char *)ifi + NLMSG_ALIGN_NL(sizeof(*ifi)));
                 RTA_OK_NL(a, len); a = RTA_NEXT_NL(a, len)) {
                if (a->rta_type == IFLA_IFNAME_NL) this_name = (const char *)RTA_DATA_NL(a);
                if (a->rta_type == IFLA_BROADCAST_NL && RTA_PAYLOAD_NL(a) >= 6) {
                    memcpy(bcast, RTA_DATA_NL(a), 6);
                    saw_bcast_attr = 1;
                }
            }
            if (this_name && strcmp(this_name, want) == 0) {
                found = 1;
                is_ether = (ifi->ifi_type == ARPHRD_ETHER);
                has_bcast_flag = (ifi->ifi_flags & IFF_BROADCAST_NL) != 0;
                bcast_all_ff = saw_bcast_attr && bcast[0] == 0xff && bcast[1] == 0xff &&
                               bcast[2] == 0xff && bcast[3] == 0xff && bcast[4] == 0xff &&
                               bcast[5] == 0xff;
            }
        }
    }

    CHECK(found, "RTM_GETLINK dump includes the TAP interface");
    CHECK(is_ether, "TAP RTM_GETLINK reports ARPHRD_ETHER link type");
    CHECK(has_bcast_flag, "TAP RTM_GETLINK reports IFF_BROADCAST");
    CHECK(bcast_all_ff, "TAP IFLA_BROADCAST is ff:ff:ff:ff:ff:ff");
}

// Dump RTM_GETADDR and assert the TAP's IPv4 carries an IFA_BROADCAST attribute
// (ether_setup gives tap interfaces an IPv4 broadcast, unlike a point-to-point
// TUN). `want` is the interface name learned from TUNSETIFF.
static void nl_check_tap_getaddr(int fd, const char *want) {
    char buf[8192];
    int found = 0, has_bcast = 0, done = 0;

    while (!done) {
        ssize_t n = read(fd, buf, sizeof(buf));
        if (n < 0) {
            CHECK(0, "read RTM_GETADDR dump for TAP");
            return;
        }
        int rem = (int)n;
        for (struct nlmsghdr_nl *nh = (struct nlmsghdr_nl *)buf; NLMSG_OK_NL(nh, rem);
             nh = NLMSG_NEXT_NL(nh, rem)) {
            if (nh->nlmsg_type == NLMSG_DONE_NL) { done = 1; break; }
            if (nh->nlmsg_type != RTM_NEWADDR_NL) continue;
            struct ifaddrmsg_nl *ifa = NLMSG_DATA_NL(nh);
            const char *this_label = NULL;
            int saw_bcast = 0;
            int len = (int)nh->nlmsg_len - NLMSG_HDRLEN_NL - (int)sizeof(*ifa);
            for (struct rtattr_nl *a = (struct rtattr_nl *)((char *)ifa + NLMSG_ALIGN_NL(sizeof(*ifa)));
                 RTA_OK_NL(a, len); a = RTA_NEXT_NL(a, len)) {
                if (a->rta_type == IFA_LABEL_NL) this_label = (const char *)RTA_DATA_NL(a);
                if (a->rta_type == IFA_BROADCAST_NL && RTA_PAYLOAD_NL(a) >= 4) saw_bcast = 1;
            }
            if (this_label && strcmp(this_label, want) == 0) {
                found = 1;
                has_bcast = saw_bcast;
            }
        }
    }

    CHECK(found, "RTM_GETADDR dump includes the TAP IPv4 address");
    CHECK(has_bcast, "TAP RTM_GETADDR carries an IFA_BROADCAST attribute");
}

static void test_tap_rtnetlink_broadcast(void) {
    int fd = open_tun();
    if (fd < 0) return;

    char name[IFNAMSIZ] = {0};
    CHECK_RET(tun_setiff(fd, "tstap4", IFF_TAP | IFF_NO_PI, name), 0,
              "TUNSETIFF IFF_TAP on tstap4 for rtnetlink broadcast test");

    // Give the TAP an IPv4 so RTM_GETADDR has an address to report (and, per
    // ether_setup, an IPv4 broadcast). Same SIOCSIFADDR path the other tests use.
    int csock = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(csock >= 0, "control socket for TAP rtnetlink test");
    if (csock >= 0) {
        struct ifreq ifr_a;
        memset(&ifr_a, 0, sizeof(ifr_a));
        set_ifname(ifr_a.ifr_name, name);
        struct sockaddr_in *sa = (struct sockaddr_in *)&ifr_a.ifr_addr;
        sa->sin_family = AF_INET;
        sa->sin_addr.s_addr = htonl(0x0A450001u); /* 10.69.0.1 */
        CHECK_RET(ioctl(csock, SIOCSIFADDR, &ifr_a), 0,
                  "SIOCSIFADDR 10.69.0.1/24 on tstap4");
        close(csock);
    }

    int nl = socket(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE_NL);
    CHECK(nl >= 0, "open NETLINK_ROUTE socket for TAP broadcast attrs");
    if (nl < 0) { close(fd); return; }

    struct sockaddr_nl_nl addr;
    memset(&addr, 0, sizeof(addr));
    addr.nl_family = AF_NETLINK;
    CHECK(bind(nl, (struct sockaddr *)&addr, sizeof(addr)) == 0,
          "bind NETLINK_ROUTE socket");

    CHECK(nl_send_dump(nl, RTM_GETLINK_NL, AF_PACKET, 11),
          "send RTM_GETLINK dump request");
    nl_check_tap_getlink(nl, name);

    CHECK(nl_send_dump(nl, RTM_GETADDR_NL, AF_INET, 12),
          "send RTM_GETADDR dump request");
    nl_check_tap_getaddr(nl, name);

    close(nl);
    close(fd);
}

// Concurrent same-fd TUNSETIFF serialization + TUNSETIFF/close race.
//
// set_iff() serializes concurrent TUNSETIFF on one fd with the `setting_iff`
// atomic (Linux uses tun_lock/rtnl_lock): the check `attached.is_none()` and the
// write-back of the chosen device are not atomic on their own, so two callers
// that both see "unattached" would each create/find a device and the second
// write-back would leak the first. And close() flips a `closing` latch that the
// racing TUNSETIFF re-checks before recording the attachment, rolling a
// just-created device back instead of stranding it on a dying fd.
//
// This drives both from userspace with N threads sharing one fd:
//   Part 1 - N threads hammer TUNSETIFF on the same fd with the same name.
//            Exactly one must win (ret 0); the rest get EEXIST (already bound)
//            or EBUSY (serialization). Never two winners, never a leak: a
//            second independent fd must still see EBUSY, and after close a fresh
//            create must succeed (name not stranded).
//   Part 2 - one thread closes the shared fd while others issue TUNSETIFF on it.
//            The observable outcome must stay clean: no thread reports a second
//            successful bind, and the name is recreatable afterward.
//
// Without the serialization/closing guard this flips red: concurrent winners
// leak a device (second fd stops returning EBUSY, or the name strands in EBUSY
// and cannot be recreated).

#define RACE_THREADS 4

struct setiff_race_arg {
    int fd;
    const char *name;
    pthread_barrier_t *start;
    int ret;   // TUNSETIFF return value observed by this thread
    int err;   // errno observed by this thread
};

static void *setiff_race_worker(void *p) {
    struct setiff_race_arg *a = p;
    pthread_barrier_wait(a->start);
    struct ifreq ifr;
    memset(&ifr, 0, sizeof(ifr));
    set_ifname(ifr.ifr_name, a->name);
    ifr.ifr_flags = IFF_TUN | IFF_NO_PI;
    errno = 0;
    a->ret = ioctl(a->fd, TUNSETIFF, &ifr);
    a->err = errno;
    return NULL;
}

// Part 1: concurrent TUNSETIFF on the same fd must produce exactly one winner
// and never leak a second device.
static void race_same_fd_setiff(void) {
    int fd = open_tun();
    if (fd < 0) return;

    pthread_barrier_t start;
    pthread_barrier_init(&start, NULL, RACE_THREADS);
    pthread_t th[RACE_THREADS];
    struct setiff_race_arg args[RACE_THREADS];

    for (int i = 0; i < RACE_THREADS; i++) {
        args[i].fd = fd;
        args[i].name = "tstrace";
        args[i].start = &start;
        args[i].ret = -2;
        args[i].err = 0;
    }
    int spawned = 0;
    for (int i = 0; i < RACE_THREADS; i++) {
        if (pthread_create(&th[i], NULL, setiff_race_worker, &args[i]) == 0) {
            spawned++;
        } else {
            args[i].ret = -3; // mark not-spawned so it is excluded from the tally
        }
    }
    for (int i = 0; i < RACE_THREADS; i++) {
        if (args[i].ret != -3) pthread_join(th[i], NULL);
    }
    pthread_barrier_destroy(&start);

    CHECK(spawned == RACE_THREADS, "spawned all concurrent TUNSETIFF racers");

    int winners = 0, clean_losers = 0;
    for (int i = 0; i < RACE_THREADS; i++) {
        if (args[i].ret == -3) continue;
        if (args[i].ret == 0) {
            winners++;
        } else if (args[i].ret == -1 && (args[i].err == EEXIST || args[i].err == EBUSY)) {
            clean_losers++;
        }
    }
    // Exactly one thread may bind the fd; the serialization forbids a second
    // winner (which would leak a device).
    CHECK(winners == 1, "exactly one concurrent TUNSETIFF wins on the shared fd");
    CHECK(clean_losers == spawned - 1,
          "all losing racers fail cleanly with EEXIST/EBUSY (no leak)");

    // The single bound device must own its single queue: a second independent
    // fd sees EBUSY. Two leaked devices, or a stranded claim, would break this.
    int fd2 = open_tun();
    if (fd2 >= 0) {
        CHECK_ERR(tun_setiff(fd2, "tstrace", IFF_TUN | IFF_NO_PI, NULL), EBUSY,
                  "second fd on the raced device returns EBUSY (single attachment)");
        close(fd2);
    }
    close(fd);

    // After the winner's fd closes, the non-persistent device must be gone and
    // the name reusable - proof nothing leaked or stranded during the race.
    int fd3 = open_tun();
    if (fd3 >= 0) {
        CHECK_RET(tun_setiff(fd3, "tstrace", IFF_TUN | IFF_NO_PI, NULL), 0,
                  "raced name is recreatable after close (not stranded)");
        close(fd3);
    }
}

struct close_race_arg {
    int fd;
    pthread_barrier_t *start;
};

static void *close_race_closer(void *p) {
    struct close_race_arg *a = p;
    pthread_barrier_wait(a->start);
    close(a->fd);
    return NULL;
}

// Part 2: a close on the shared fd racing TUNSETIFF must never leave the name
// stranded. Repeated rounds maximize the chance of hitting the window between
// try_attach() and recording the attachment.
static void race_setiff_vs_close(void) {
    const int rounds = 16;
    for (int r = 0; r < rounds; r++) {
        int fd = open_tun();
        if (fd < 0) return;

        pthread_barrier_t start;
        pthread_barrier_init(&start, NULL, 2);
        struct close_race_arg carg = {.fd = fd, .start = &start};
        pthread_t closer;
        int have_closer = pthread_create(&closer, NULL, close_race_closer, &carg) == 0;

        // Race the closer: issue TUNSETIFF on the same fd right as it closes.
        pthread_barrier_wait(&start);
        struct ifreq ifr;
        memset(&ifr, 0, sizeof(ifr));
        set_ifname(ifr.ifr_name, "tstclr");
        ifr.ifr_flags = IFF_TUN | IFF_NO_PI;
        errno = 0;
        int r1 = ioctl(fd, TUNSETIFF, &ifr);
        // A successful TUNSETIFF must have bound the real device; a lost race is
        // reported as EBADFD/EBADF/EINVAL. Any of these is a clean outcome; the
        // only thing forbidden is a stranded name (checked after the loop).
        (void)r1;

        if (have_closer) pthread_join(closer, NULL);
        pthread_barrier_destroy(&start);
        // The winner (if TUNSETIFF bound before close ran) leaves a live fd; if
        // close already ran, fd is gone. Close defensively - double close on an
        // already-closed fd just returns EBADF.
        close(fd);
    }

    // The decisive assertion: after every TUNSETIFF/close interleaving, the name
    // must be free to recreate. A stranded Attached claim (missing closing-guard
    // rollback) would make this EBUSY.
    int fd = open_tun();
    if (fd >= 0) {
        CHECK_RET(tun_setiff(fd, "tstclr", IFF_TUN | IFF_NO_PI, NULL), 0,
                  "name is recreatable after TUNSETIFF/close race rounds (not stranded)");
        close(fd);
    }
}

static void test_setiff_close_race(void) {
    race_same_fd_setiff();
    race_setiff_vs_close();
}

// TUNSETIFF CAP_NET_ADMIN de-privilege regression (old-red / new-green).
//
// Linux tun_set_iff() gates creating/attaching a TUN/TAP device on
// CAP_NET_ADMIN (tun.c). set_iff_inner() enforces the same:
//   if !cred().has_cap_net_admin() { return EPERM }.
// Pre-gate code (no cap check) would let an unprivileged caller create the
// device -> TUNSETIFF returns 0 (RED for this assertion); with the gate an
// unprivileged caller gets EPERM (GREEN).
//
// StarryOS has no per-thread capset path that can clear a single effective cap
// while staying uid 0 (capset from root is a no-op / EPERM, per the capset
// test), so we drop privilege the way Linux honors: setuid() from root to a
// non-root uid, which runs apply_id_change_capability_rules (cred.rs) and, for
// the all-root -> all-non-root transition, zeroes cap_permitted/cap_effective.
// That is verified independently by the capset test's setuid_clears_caps case.
// We do this in a forked child so the parent (and later tests) keep their caps.
// Result the de-privileged child reports back to the parent over a pipe, so the
// parent - not the child - makes the EPERM determination as a counted CHECK.
// `stage` records how far the child got; `ret`/`err` are the observed TUNSETIFF
// result so the parent asserts the exact `ret == -1 && errno == EPERM` itself.
struct cap_result {
    int stage; // 0=setuid failed, 1=euid wrong, 2=open failed, 3=ioctl attempted
    int ret;   // ioctl() return value (stage 3 only)
    int err;   // errno after ioctl()   (stage 3 only)
};

static void cap_net_admin_eperm_child(int wfd) {
    struct cap_result res;
    memset(&res, 0, sizeof(res));

    // Must run before we create any interface: prove that after dropping all
    // capabilities, TUNSETIFF is rejected with EPERM.
    if (setuid(1000) != 0) {
        res.stage = 0;
        (void)!write(wfd, &res, sizeof(res));
        _exit(0);
    }
    if (geteuid() != 1000) {
        res.stage = 1;
        (void)!write(wfd, &res, sizeof(res));
        _exit(0);
    }

    int fd = open("/dev/net/tun", O_RDWR);
    if (fd < 0) {
        res.stage = 2;
        (void)!write(wfd, &res, sizeof(res));
        _exit(0);
    }

    struct ifreq ifr;
    memset(&ifr, 0, sizeof(ifr));
    set_ifname(ifr.ifr_name, "tstunp");
    ifr.ifr_flags = IFF_TUN | IFF_NO_PI;
    errno = 0;
    int r = ioctl(fd, TUNSETIFF, &ifr);
    res.stage = 3;
    res.ret = r;
    res.err = errno;
    (void)!write(wfd, &res, sizeof(res));
    close(fd);
    _exit(0);
}

static void test_setiff_cap_net_admin_eperm(void) {
    int pipefd[2];
    if (pipe(pipefd) != 0) {
        CHECK(0, "pipe for CAP_NET_ADMIN de-privilege test");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        close(pipefd[0]);
        cap_net_admin_eperm_child(pipefd[1]);
        _exit(0); /* unreachable */
    }
    close(pipefd[1]);
    if (pid < 0) {
        CHECK(0, "fork for CAP_NET_ADMIN de-privilege test");
        close(pipefd[0]);
        return;
    }

    // Read the child's observed result and let the PARENT decide, so the EPERM
    // outcome is a parent-visible CHECK counted in the DONE total (not hidden in
    // the child's exit status). The child's own view is advisory only.
    struct cap_result res;
    memset(&res, 0, sizeof(res));
    ssize_t got = read(pipefd[0], &res, sizeof(res));
    close(pipefd[0]);

    int status = 0;
    if (waitpid(pid, &status, 0) != pid) {
        CHECK(0, "waitpid for CAP_NET_ADMIN child");
        return;
    }

    // The child must have reached the ioctl (stage 3); earlier stages mean the
    // de-privileging environment itself failed, which invalidates the test.
    CHECK(got == (ssize_t)sizeof(res) && res.stage == 3,
          "de-privileged child reached TUNSETIFF (setuid(1000) dropped caps)");
    if (got != (ssize_t)sizeof(res) || res.stage != 3) {
        return;
    }
    // Parent-visible assertion of the actual EPERM gate. Pre-gate code (no cap
    // check) lets the unprivileged create through -> ret==0 (RED); the gate
    // yields ret==-1/errno==EPERM (GREEN).
    CHECK(res.ret == -1 && res.err == EPERM,
          "TUNSETIFF without CAP_NET_ADMIN returns EPERM (parent-asserted)");
}

// TUNSETIFF non-stranding regression for the failed-bind rollback.
//
// set_iff_inner() claims the interface's single queue with try_attach() BEFORE
// writing the name back to userspace. If a later step fails after the claim,
// rollback_claim() must undo it (detach a pre-existing device / destroy a
// just-created one). Without rollback the name would be pinned in Attached and
// every subsequent TUNSETIFF on it would return EBUSY forever, or the device
// would leak.
//
// The specific rollback trigger the fix targets - write_ifr_name() faulting on
// a bad ifr pointer - is NOT reachable deterministically from single-threaded
// userspace: read_ifr_name() and read_ifr_flags() validate the SAME ifreq
// region (offset 0..18) with READ|WRITE access up front (mm/access.rs
// UserPtr::get_as_mut, ACCESS_FLAGS = READ|WRITE), and write_ifr_name() writes a
// strict subset (offset 0..16). Any pointer bad enough to fault the write-back
// faults the initial read first, so set_iff_inner() returns EFAULT before ever
// calling try_attach(). Faulting only the write-back requires a concurrent
// mprotect/fork COW re-marking the page read-only between the check and the
// write - a race a lone userspace thread cannot force. So this test instead
// verifies the rollback GUARANTEE through a reachable failure: a TUNSETIFF that
// fails on the same fd must not strand the name.
//
// Sequence:
//   1. Create tstunr (a real TUN device now exists, attached to fd1).
//   2. On fd1, issue a SECOND TUNSETIFF -> EEXIST (already-bound; the check runs
//      before try_attach so tstunr's single claim is untouched).
//   3. Close fd1 -> non-persistent device is destroyed.
//   4. Re-create tstunr on a fresh fd -> must succeed (0), not EBUSY. A stale
//      Attached claim (missing detach/rollback) or a leaked device would make
//      this return EBUSY / EEXIST-class error.
static void test_setiff_no_strand_after_failed_bind(void) {
    int fd1 = open_tun();
    if (fd1 < 0) return;

    CHECK_RET(tun_setiff(fd1, "tstunr", IFF_TUN | IFF_NO_PI, NULL), 0,
              "create tstunr on fd1");
    // Second TUNSETIFF on the already-bound fd fails (EEXIST) without disturbing
    // the queue claim: the already-bound check precedes try_attach.
    CHECK_ERR(tun_setiff(fd1, "tstunr", IFF_TUN | IFF_NO_PI, NULL), EEXIST,
              "second TUNSETIFF on bound fd returns EEXIST (claim untouched)");
    close(fd1);

    // The name must be fully released: a fresh create must succeed rather than
    // hit a stranded Attached slot (EBUSY) or a leaked device.
    int fd2 = open_tun();
    if (fd2 < 0) return;
    CHECK_RET(tun_setiff(fd2, "tstunr", IFF_TUN | IFF_NO_PI, NULL), 0,
              "recreate tstunr after failed second bind + close (not stranded)");
    close(fd2);

    // And a second independent fd on the freshly recreated device still yields
    // EBUSY, proving the claim mechanism is intact (not permanently broken by
    // the earlier failure).
    int fd3 = open_tun();
    int fd4 = open_tun();
    if (fd3 < 0 || fd4 < 0) { close(fd3); close(fd4); return; }
    CHECK_RET(tun_setiff(fd3, "tstunr2", IFF_TUN | IFF_NO_PI, NULL), 0,
              "create tstunr2 on fd3");
    CHECK_ERR(tun_setiff(fd4, "tstunr2", IFF_TUN | IFF_NO_PI, NULL), EBUSY,
              "second fd on tstunr2 still returns EBUSY (claim intact)");
    close(fd3);
    close(fd4);
}

// TUN PI short-buffer read: when the user buffer is too small for the full
// kernel→user IP packet, the PI header flags field must have TUN_PKT_STRIP
// (0x0001) set (Linux tun.c:2093 `pi.flags |= TUN_PKT_STRIP`).
// Triggers by routing a 200-byte UDP datagram through the TUN interface so
// the kernel enqueues an IP/UDP packet (~232 B) in the tx_queue; reading
// with a 19-byte buffer (4 PI + 15 payload) forces the truncation path.
static void test_tun_pi_short_read(void) {
    int fd = open_tun();
    if (fd < 0) return;

    char name[IFNAMSIZ] = {};
    CHECK_RET(tun_setiff(fd, "tstun9", IFF_TUN, name), 0,
              "TUNSETIFF IFF_TUN (PI mode) on tstun9");

    int csock = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(csock >= 0, "control socket for ioctl");
    if (csock < 0) { close(fd); return; }

    /* SIOCSIFADDR: 10.66.0.1/24 — also installs the connected route */
    struct ifreq ifr_a = {};
    set_ifname(ifr_a.ifr_name, name);
    struct sockaddr_in *sa = (struct sockaddr_in *)&ifr_a.ifr_addr;
    sa->sin_family = AF_INET;
    sa->sin_addr.s_addr = htonl(0x0A420001u); /* 10.66.0.1 */
    CHECK_RET(ioctl(csock, SIOCSIFADDR, &ifr_a), 0,
              "SIOCSIFADDR 10.66.0.1/24 on tstun9");

    /* Bring the interface UP so the route is eligible */
    struct ifreq ifr_f = {};
    set_ifname(ifr_f.ifr_name, name);
    ifr_f.ifr_flags = IFF_UP;
    CHECK_RET(ioctl(csock, SIOCSIFFLAGS, &ifr_f), 0,
              "SIOCSIFFLAGS IFF_UP on tstun9");

    /* Send UDP to 10.66.0.2; kernel routes it through tstun9 → tx_queue */
    int usock = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(usock >= 0, "UDP socket for sendto");
    if (usock < 0) { close(csock); close(fd); return; }

    struct sockaddr_in dst = {};
    dst.sin_family = AF_INET;
    dst.sin_addr.s_addr = htonl(0x0A420002u); /* 10.66.0.2 */
    dst.sin_port = htons(9999);
    /* 200-byte payload → IP/UDP packet ~228 B >> 15-byte read window */
    char payload[200];
    memset(payload, 'A', sizeof(payload));
    ssize_t sent = sendto(usock, payload, sizeof(payload), 0,
                          (struct sockaddr *)&dst, sizeof(dst));
    CHECK(sent == (ssize_t)sizeof(payload),
          "sendto routes 200-B UDP through tstun9");
    if (sent != (ssize_t)sizeof(payload)) { close(usock); close(csock); close(fd); return; }

    /* Wait for the net_poll_worker to route and enqueue the packet */
    struct pollfd pfd = { .fd = fd, .events = POLLIN };
    fflush(stdout);
    int pr = poll(&pfd, 1, 2000);
    CHECK(pr == 1 && (pfd.revents & POLLIN),
          "tstun9 fd becomes readable after kernel routes UDP");
    if (pr != 1 || !(pfd.revents & POLLIN)) { close(usock); close(csock); close(fd); return; }

    /* Short read: 4 PI + 15 bytes = 19 total; real packet is much larger */
    unsigned char short_buf[TUN_PI_LEN + 15];
    memset(short_buf, 0, sizeof(short_buf));
    ssize_t n = read(fd, short_buf, sizeof(short_buf));
    CHECK(n == (ssize_t)sizeof(short_buf),
          "TUN PI short read returns exactly the requested byte count");

    uint16_t pi_flags;
    memcpy(&pi_flags, short_buf, sizeof(pi_flags));
    CHECK(pi_flags & TUN_PKT_STRIP,
          "TUN_PKT_STRIP set in PI flags when user buf < full IP packet");

    close(usock);
    close(csock);
    close(fd);
}

// TAP PI short-buffer read: same TUN_PKT_STRIP path for an EthernetDevice-
// backed TAP. Sending UDP to 10.67.0.2 triggers L2 ARP resolution; the kernel
// enqueues a 42-byte ARP request frame into the TAP tx_queue. Reading with a
// 19-byte buffer (4 PI + 15 payload) forces the truncation flag.
static void test_tap_pi_short_read(void) {
    int fd = open_tun();
    if (fd < 0) return;

    char name[IFNAMSIZ] = {};
    CHECK_RET(tun_setiff(fd, "tstap5", IFF_TAP, name), 0,
              "TUNSETIFF IFF_TAP (PI mode) on tstap5");

    int csock = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(csock >= 0, "control socket for ioctl");
    if (csock < 0) { close(fd); return; }

    struct ifreq ifr_a = {};
    set_ifname(ifr_a.ifr_name, name);
    struct sockaddr_in *sa = (struct sockaddr_in *)&ifr_a.ifr_addr;
    sa->sin_family = AF_INET;
    sa->sin_addr.s_addr = htonl(0x0A430001u); /* 10.67.0.1 */
    CHECK_RET(ioctl(csock, SIOCSIFADDR, &ifr_a), 0,
              "SIOCSIFADDR 10.67.0.1/24 on tstap5");

    struct ifreq ifr_f = {};
    set_ifname(ifr_f.ifr_name, name);
    ifr_f.ifr_flags = IFF_UP;
    CHECK_RET(ioctl(csock, SIOCSIFFLAGS, &ifr_f), 0,
              "SIOCSIFFLAGS IFF_UP on tstap5");

    int usock = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(usock >= 0, "UDP socket for sendto");
    if (usock < 0) { close(csock); close(fd); return; }

    struct sockaddr_in dst = {};
    dst.sin_family = AF_INET;
    dst.sin_addr.s_addr = htonl(0x0A430002u); /* 10.67.0.2 */
    dst.sin_port = htons(9999);
    char payload[50];
    memset(payload, 'B', sizeof(payload));
    ssize_t sent = sendto(usock, payload, sizeof(payload), 0,
                          (struct sockaddr *)&dst, sizeof(dst));
    CHECK(sent == (ssize_t)sizeof(payload),
          "sendto to 10.67.0.2 triggers ARP through tstap5");
    if (sent != (ssize_t)sizeof(payload)) { close(usock); close(csock); close(fd); return; }

    /* ARP request frame (42 B) + PI (4 B) = 46 B; arrives in TAP tx_queue */
    struct pollfd pfd = { .fd = fd, .events = POLLIN };
    fflush(stdout);
    int pr = poll(&pfd, 1, 2000);
    CHECK(pr == 1 && (pfd.revents & POLLIN),
          "tstap5 fd becomes readable (kernel generated ARP request)");
    if (pr != 1 || !(pfd.revents & POLLIN)) { close(usock); close(csock); close(fd); return; }

    /* Short buffer: 4 PI + 15 payload = 19 < 46 → TUN_PKT_STRIP must be set */
    unsigned char short_buf[TUN_PI_LEN + 15];
    memset(short_buf, 0, sizeof(short_buf));
    ssize_t n = read(fd, short_buf, sizeof(short_buf));
    CHECK(n == (ssize_t)sizeof(short_buf),
          "TAP PI short read returns exactly the requested byte count");

    uint16_t pi_flags;
    memcpy(&pi_flags, short_buf, sizeof(pi_flags));
    CHECK(pi_flags & TUN_PKT_STRIP,
          "TUN_PKT_STRIP set in TAP PI flags when user buf < ARP frame");

    close(usock);
    close(csock);
    close(fd);
}

// Per-interface MTU datapath enforcement, WRITE direction (fd -> stack), TUN.
//
// SIOCSIFMTU must not be a pure control-plane query field: after LOWERING the
// MTU, an oversize inject must be dropped at the per-interface boundary,
// mirroring Linux where dev->mtu is per-net_device. Linux tun_get_user does not
// error an over-MTU inject; write() still returns the full length. The frame is
// dropped silently in TunShared::push_rx against frame_limit = mtu.
//
// The observation is /proc/net/dev rx_packets DELTA, not socket delivery: the
// router device verifies RX checksums (ChecksumCapabilities defaults to Both),
// so a raw injected packet with a zero IP/UDP checksum would be dropped at
// checksum verification regardless of MTU, making socket delivery unable to
// distinguish an MTU drop from a checksum drop. rx_packets increments at
// queue-drain time (TunDevice::recv -> count_rx), strictly upstream of checksum
// verification, so it fires for any frame push_rx admitted and never for one it
// dropped. Boundary: exactly MTU passes, MTU+1 drops, MTU-1 passes.
//
// Pre-fix push_rx only bounds at TUN_FRAME_CAPACITY (65549), so the oversize
// inject is admitted, drained, and increments rx_packets -> the "no increment"
// assertion RED-fails. Post-fix it is dropped against frame_limit=1000 -> GREEN.
static void test_mtu_datapath_enforcement_tun_write(void) {
    int fd = open_tun();
    if (fd < 0) return;

    char name[IFNAMSIZ] = {};
    // IFF_NO_PI: write() carries the bare IP packet with no 4-byte tun_pi header,
    // so lengths compare directly against the MTU.
    CHECK_RET(tun_setiff(fd, "tstunmw", IFF_TUN | IFF_NO_PI, name), 0,
              "TUNSETIFF IFF_TUN|IFF_NO_PI on tstunmw succeeds");

    int csock = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(csock >= 0, "control socket for TUN write-MTU test");
    if (csock < 0) { close(fd); return; }

    struct ifreq ifr_a = {};
    set_ifname(ifr_a.ifr_name, name);
    struct sockaddr_in *sa = (struct sockaddr_in *)&ifr_a.ifr_addr;
    sa->sin_family = AF_INET;
    sa->sin_addr.s_addr = htonl(0x0A470001u); /* 10.71.0.1 */
    CHECK_RET(ioctl(csock, SIOCSIFADDR, &ifr_a), 0,
              "SIOCSIFADDR 10.71.0.1/24 on tstunmw");

    /* Lower the MTU to 1000. After this, an inject > 1000 B must be dropped. */
    struct ifreq ifr_m = {};
    set_ifname(ifr_m.ifr_name, name);
    ifr_m.ifr_mtu = 1000;
    CHECK_RET(ioctl(csock, SIOCSIFMTU, &ifr_m), 0,
              "SIOCSIFMTU 1000 (lowered) on tstunmw");

    struct ifreq ifr_g = {};
    set_ifname(ifr_g.ifr_name, name);
    CHECK_RET(ioctl(csock, SIOCGIFMTU, &ifr_g), 0, "SIOCGIFMTU on tstunmw");
    CHECK(ifr_g.ifr_mtu == 1000, "SIOCGIFMTU reads back the lowered MTU (1000)");

    struct ifreq ifr_f = {};
    set_ifname(ifr_f.ifr_name, name);
    ifr_f.ifr_flags = IFF_UP;
    CHECK_RET(ioctl(csock, SIOCSIFFLAGS, &ifr_f), 0,
              "SIOCSIFFLAGS IFF_UP on tstunmw");

    /* One IPv4 datagram of `total` bytes addressed 10.71.0.2 -> 10.71.0.1.
       Only the IPv4 total-length header must equal `total`; the payload content
       is irrelevant because the observation is rx_packets, not delivery. */
    unsigned char buf[2048];

    /* rx_packets baseline. -1 means the line is absent (interface has never
       received); treat that as 0 for delta purposes. */
    long base = read_rx_packets(name);
    long rx0 = base < 0 ? 0 : base;

    /* --- MTU-1 (999 B): must be ADMITTED -> rx_packets increments by 1. --- */
    memset(buf, 0, sizeof(buf));
    buf[0] = 0x45; buf[2] = (unsigned char)(999 >> 8); buf[3] = (unsigned char)(999 & 0xff);
    buf[8] = 64; buf[9] = 17;
    buf[12] = 10; buf[13] = 71; buf[14] = 0; buf[15] = 2;
    buf[16] = 10; buf[17] = 71; buf[18] = 0; buf[19] = 1;
    ssize_t w = write(fd, buf, 999);
    CHECK(w == 999, "write() of a 999-B frame (< MTU 1000) returns full length");
    /* Give the RX worker time to drain the admitted frame. */
    fflush(stdout);
    struct pollfd wp = { .fd = csock, .events = 0 };
    (void)poll(&wp, 1, 300);
    long rx1 = read_rx_packets(name);
    CHECK(rx1 == rx0 + 1,
          "within-MTU inject (999 B < MTU 1000) is admitted: rx_packets += 1");

    /* --- exactly MTU (1000 B): must be ADMITTED -> rx_packets increments. --- */
    memset(buf, 0, sizeof(buf));
    buf[0] = 0x45; buf[2] = (unsigned char)(1000 >> 8); buf[3] = (unsigned char)(1000 & 0xff);
    buf[8] = 64; buf[9] = 17;
    buf[12] = 10; buf[13] = 71; buf[14] = 0; buf[15] = 2;
    buf[16] = 10; buf[17] = 71; buf[18] = 0; buf[19] = 1;
    w = write(fd, buf, 1000);
    CHECK(w == 1000, "write() of a 1000-B frame (== MTU 1000) returns full length");
    fflush(stdout);
    (void)poll(&wp, 1, 300);
    long rx2 = read_rx_packets(name);
    CHECK(rx2 == rx1 + 1,
          "exactly-MTU inject (1000 B == MTU 1000) is admitted: rx_packets += 1");

    /* --- MTU+1 (1001 B): must be DROPPED -> rx_packets UNCHANGED. --- */
    memset(buf, 0, sizeof(buf));
    buf[0] = 0x45; buf[2] = (unsigned char)(1001 >> 8); buf[3] = (unsigned char)(1001 & 0xff);
    buf[8] = 64; buf[9] = 17;
    buf[12] = 10; buf[13] = 71; buf[14] = 0; buf[15] = 2;
    buf[16] = 10; buf[17] = 71; buf[18] = 0; buf[19] = 1;
    w = write(fd, buf, 1001);
    /* Linux does NOT error an over-MTU inject: write() returns the full length.
       Asserting the errno-free full-length return pins that we did NOT regress
       into a non-Linux EMSGSIZE. */
    CHECK(w == 1001,
          "write() of a 1001-B frame (> MTU 1000) returns full length, no errno");
    fflush(stdout);
    (void)poll(&wp, 1, 500);
    long rx3 = read_rx_packets(name);
    CHECK(rx3 == rx2,
          "over-MTU inject (1001 B > MTU 1000) is dropped: rx_packets unchanged");

    /* --- far-oversize (1400 B): must be DROPPED -> rx_packets UNCHANGED. --- */
    memset(buf, 0, sizeof(buf));
    buf[0] = 0x45; buf[2] = (unsigned char)(1400 >> 8); buf[3] = (unsigned char)(1400 & 0xff);
    buf[8] = 64; buf[9] = 17;
    buf[12] = 10; buf[13] = 71; buf[14] = 0; buf[15] = 2;
    buf[16] = 10; buf[17] = 71; buf[18] = 0; buf[19] = 1;
    w = write(fd, buf, 1400);
    CHECK(w == 1400, "write() of a 1400-B frame (>> MTU 1000) returns full length");
    fflush(stdout);
    (void)poll(&wp, 1, 500);
    long rx4 = read_rx_packets(name);
    CHECK(rx4 == rx3,
          "far-oversize inject (1400 B > MTU 1000) is dropped: rx_packets unchanged");

    close(csock);
    close(fd);
}

// Per-interface MTU datapath enforcement, READ direction (stack -> fd), TUN.
//
// The shared smoltcp wire is pinned at STACK_MAX_MTU (65535) and cached once, so
// the core emits a single unfragmented datagram larger than a lowered interface
// MTU. TunDevice::send must drop such a frame (NET_XMIT_DROP, no errno) so it
// never surfaces at read(2), matching the IP layer refusing to xmit an over-MTU
// skb. Pre-fix send() queues it, the fd becomes readable, read() returns the
// oversize length -> RED. Boundary and raise-MTU re-admission are both pinned.
static void test_mtu_datapath_enforcement_tun_read(void) {
    int fd = open_tun();
    if (fd < 0) return;

    char name[IFNAMSIZ] = {};
    CHECK_RET(tun_setiff(fd, "tstunmr", IFF_TUN | IFF_NO_PI, name), 0,
              "TUNSETIFF IFF_TUN|IFF_NO_PI on tstunmr succeeds");

    int csock = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(csock >= 0, "control socket for TUN read-MTU test");
    if (csock < 0) { close(fd); return; }

    struct ifreq ifr_a = {};
    set_ifname(ifr_a.ifr_name, name);
    struct sockaddr_in *sa = (struct sockaddr_in *)&ifr_a.ifr_addr;
    sa->sin_family = AF_INET;
    sa->sin_addr.s_addr = htonl(0x0A480001u); /* 10.72.0.1 */
    CHECK_RET(ioctl(csock, SIOCSIFADDR, &ifr_a), 0,
              "SIOCSIFADDR 10.72.0.1/24 on tstunmr");

    /* Lower the MTU to 1000. */
    struct ifreq ifr_m = {};
    set_ifname(ifr_m.ifr_name, name);
    ifr_m.ifr_mtu = 1000;
    CHECK_RET(ioctl(csock, SIOCSIFMTU, &ifr_m), 0,
              "SIOCSIFMTU 1000 (lowered) on tstunmr");

    struct ifreq ifr_f = {};
    set_ifname(ifr_f.ifr_name, name);
    ifr_f.ifr_flags = IFF_UP;
    CHECK_RET(ioctl(csock, SIOCSIFFLAGS, &ifr_f), 0,
              "SIOCSIFFLAGS IFF_UP on tstunmr");

    int usock = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(usock >= 0, "UDP socket for read-MTU sendto");
    if (usock < 0) { close(csock); close(fd); return; }
    struct sockaddr_in dst = {};
    dst.sin_family = AF_INET;
    dst.sin_addr.s_addr = htonl(0x0A480002u); /* 10.72.0.2 (on-link peer) */
    dst.sin_port = htons(9999);

    /* --- within-MTU egress: 960-B payload -> 20+8+960 = 988 B <= MTU 1000.
           Must surface at read(2) (control: the drop below is MTU-specific). --- */
    static char small[960];
    memset(small, 'm', sizeof(small));
    ssize_t sent = sendto(usock, small, sizeof(small), 0,
                          (struct sockaddr *)&dst, sizeof(dst));
    CHECK(sent == (ssize_t)sizeof(small),
          "sendto routes a 960-B UDP datagram (988-B frame) toward tstunmr");
    struct pollfd pfd = { .fd = fd, .events = POLLIN };
    fflush(stdout);
    int pr = poll(&pfd, 1, 2000);
    CHECK(pr == 1 && (pfd.revents & POLLIN),
          "within-MTU egress frame (988 B <= MTU 1000) becomes readable");
    if (pr == 1 && (pfd.revents & POLLIN)) {
        static unsigned char rb[4096];
        ssize_t rn = read(fd, rb, sizeof(rb));
        CHECK(rn == 988, "within-MTU egress frame is delivered whole (988 B)");
    }

    /* --- oversize egress: 2000-B payload -> 20+8+2000 = 2028 B > MTU 1000.
           The wire is 65535 so the core emits ONE unfragmented 2028-B datagram;
           TunDevice::send must drop it, so the fd must NOT become readable. --- */
    static char payload[2000];
    memset(payload, 'M', sizeof(payload));
    sent = sendto(usock, payload, sizeof(payload), 0,
                  (struct sockaddr *)&dst, sizeof(dst));
    CHECK(sent == (ssize_t)sizeof(payload),
          "sendto routes a 2000-B UDP datagram (2028-B frame) toward tstunmr");
    pfd.revents = 0;
    fflush(stdout);
    pr = poll(&pfd, 1, 1000);
    CHECK(!(pr == 1 && (pfd.revents & POLLIN)),
          "oversize egress frame (2028 B > MTU 1000) never becomes readable");
    /* If (pre-fix) it did leak through, drain it so no stray frame poisons the
       raise-MTU sub-test below. */
    if (pr == 1 && (pfd.revents & POLLIN)) {
        static unsigned char leak[4096];
        ssize_t ln = read(fd, leak, sizeof(leak));
        CHECK(ln <= 1000,
              "any frame delivered to read(2) must be within the configured MTU");
    }

    /* --- RAISE the MTU to 3000: the SAME 2028-B datagram now surfaces whole,
           proving the read-side drop is MTU-specific and raise-MTU is not
           regressed (jumbo path preserved). --- */
    ifr_m.ifr_mtu = 3000;
    CHECK_RET(ioctl(csock, SIOCSIFMTU, &ifr_m), 0,
              "SIOCSIFMTU 3000 (raised) on tstunmr");
    sent = sendto(usock, payload, sizeof(payload), 0,
                  (struct sockaddr *)&dst, sizeof(dst));
    CHECK(sent == (ssize_t)sizeof(payload), "sendto re-routes 2000-B datagram");
    pfd.revents = 0;
    fflush(stdout);
    pr = poll(&pfd, 1, 2000);
    CHECK(pr == 1 && (pfd.revents & POLLIN),
          "after raising MTU to 3000, the 2028-B egress frame becomes readable");
    if (pr == 1 && (pfd.revents & POLLIN)) {
        static unsigned char rb[4096];
        ssize_t rn = read(fd, rb, sizeof(rb));
        CHECK(rn == 2028,
              "raised-MTU egress frame is delivered whole (2028 B), no regression");
    }

    close(usock);
    close(csock);
    close(fd);
}

// Per-interface MTU datapath enforcement for TAP (the +14 Ethernet-header path).
//
// A TAP is layer-2: the MTU bounds the payload, and the frame carries a 14-byte
// Ethernet header, so the frame ceiling is mtu + ETH_HLEN. The kernel gates the
// TAP MTU at 68..65521 (= MAX_MTU - ETH_HLEN), and TunShared stores
// frame_limit = mtu + 14. This exercises the code-distinct TAP path in
// TunEthernetDriver::transmit (read side) and push_rx with l2_overhead=14.
//
//   * WRITE (fd -> stack): a raw Ethernet frame whose length > mtu + 14 is
//     dropped by push_rx; observed via rx_packets. Boundary is on the frame
//     length: at mtu+14 admitted, at mtu+15 dropped.
//   * READ (stack -> fd): the kernel-generated ARP reply (42 B) is well within
//     any reasonable MTU, so the read-side drop is exercised structurally by the
//     TUN read test; here we pin the TAP write-side +14 boundary, which is the
//     path the CR explicitly called out and TUN does not cover.
static void test_mtu_datapath_enforcement_tap(void) {
    int fd = open_tun();
    if (fd < 0) return;

    char name[IFNAMSIZ] = {};
    // IFF_NO_PI: write() carries the bare Ethernet frame with no tun_pi header,
    // so the write length is exactly the L2 frame length.
    CHECK_RET(tun_setiff(fd, "tstapm0", IFF_TAP | IFF_NO_PI, name), 0,
              "TUNSETIFF IFF_TAP|IFF_NO_PI on tstapm0 succeeds");

    int csock = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(csock >= 0, "control socket for TAP MTU test");
    if (csock < 0) { close(fd); return; }

    /* Lower the MTU to 1000. TAP frame ceiling becomes 1000 + 14 = 1014 B. */
    struct ifreq ifr_m = {};
    set_ifname(ifr_m.ifr_name, name);
    ifr_m.ifr_mtu = 1000;
    CHECK_RET(ioctl(csock, SIOCSIFMTU, &ifr_m), 0,
              "SIOCSIFMTU 1000 (lowered) on tstapm0");

    struct ifreq ifr_g = {};
    set_ifname(ifr_g.ifr_name, name);
    CHECK_RET(ioctl(csock, SIOCGIFMTU, &ifr_g), 0, "SIOCGIFMTU on tstapm0");
    CHECK(ifr_g.ifr_mtu == 1000, "SIOCGIFMTU reads back the lowered TAP MTU (1000)");

    /* Build a valid broadcast ARP request Ethernet frame, then pad it to the
       target length. dst=broadcast so the promiscuous/own-MAC filter admits it
       and the stack drains it (rx_packets increments) rather than dropping at
       the MAC filter. ethertype ARP (0x0806); a well-formed ARP request keeps
       the frame processable so rx_packets counts it. Trailing pad bytes are
       ignored by ARP parsing but extend the L2 frame length. */
    unsigned char frame[2048];
    memset(frame, 0, sizeof(frame));
    /* dst MAC = broadcast */
    frame[0] = frame[1] = frame[2] = frame[3] = frame[4] = frame[5] = 0xff;
    /* src MAC = locally-administered unicast */
    frame[6] = 0x02; frame[7] = 0x00; frame[8] = 0xde; frame[9] = 0xad;
    frame[10] = 0xbe; frame[11] = 0xef;
    /* ethertype ARP */
    frame[12] = 0x08; frame[13] = 0x06;
    /* ARP: htype=1 ptype=0x0800 hlen=6 plen=4 op=1(request) */
    frame[14] = 0x00; frame[15] = 0x01; frame[16] = 0x08; frame[17] = 0x00;
    frame[18] = 0x06; frame[19] = 0x04; frame[20] = 0x00; frame[21] = 0x01;
    /* sender MAC */
    frame[22] = 0x02; frame[23] = 0x00; frame[24] = 0xde; frame[25] = 0xad;
    frame[26] = 0xbe; frame[27] = 0xef;
    /* sender IP 192.168.100.2 */
    frame[28] = 0xc0; frame[29] = 0xa8; frame[30] = 0x64; frame[31] = 0x02;
    /* target MAC 0 */
    /* target IP 192.168.100.1 */
    frame[38] = 0xc0; frame[39] = 0xa8; frame[40] = 0x64; frame[41] = 0x01;

    long base = read_rx_packets(name);
    long rx0 = base < 0 ? 0 : base;
    struct pollfd wp = { .fd = csock, .events = 0 };

    /* --- exactly frame ceiling (mtu + 14 = 1014 B): must be ADMITTED. --- */
    ssize_t w = write(fd, frame, 1014);
    CHECK(w == 1014,
          "TAP write of a 1014-B frame (== mtu+14) returns full length");
    fflush(stdout);
    (void)poll(&wp, 1, 300);
    long rx1 = read_rx_packets(name);
    CHECK(rx1 == rx0 + 1,
          "TAP frame at the ceiling (1014 B == mtu+14) is admitted: rx_packets += 1");

    /* --- ceiling + 1 (1015 B): must be DROPPED -> rx_packets UNCHANGED. --- */
    w = write(fd, frame, 1015);
    CHECK(w == 1015,
          "TAP write of a 1015-B frame (> mtu+14) returns full length, no errno");
    fflush(stdout);
    (void)poll(&wp, 1, 500);
    long rx2 = read_rx_packets(name);
    CHECK(rx2 == rx1,
          "TAP frame past the ceiling (1015 B > mtu+14) is dropped: rx_packets unchanged");

    /* --- far-oversize (1400 B): must be DROPPED, confirming the +14 accounting
           is not a fixed 1500 leftover. --- */
    w = write(fd, frame, 1400);
    CHECK(w == 1400, "TAP write of a 1400-B frame returns full length");
    fflush(stdout);
    (void)poll(&wp, 1, 500);
    long rx3 = read_rx_packets(name);
    CHECK(rx3 == rx2,
          "TAP far-oversize frame (1400 B > mtu+14) is dropped: rx_packets unchanged");

    close(csock);
    close(fd);
}

// Jumbo-datagram carry regression for the per-interface MTU restructure.
//
// The stack->user tx path (TunDevice::send -> tx_queue) previously held every
// frame in a fixed `[u8; STANDARD_MTU + ETH_HLEN]` (= 1514) inline array, so any
// packet the stack emitted above 1514 bytes was rejected by TunPacket::new and
// silently dropped. Raising an interface's MTU (SIOCSIFMTU) now lifts the router's
// effective MTU (capabilities().max_transmission_unit), so smoltcp emits a single
// unfragmented datagram up to that size, and the queue's heap-backed Box<[u8]>
// (sized to the actual length, ceiling STACK_MAX_MTU + ETH_HLEN = 65549) carries
// it whole. Mirrors Linux tun: dev->mtu is per-netdev and tun_net_xmit queues an
// skb sized to the frame, not a fixed 1500-MTU buffer (tun.c tun_net_init sets
// dev->mtu and dev->max_mtu = 65535, drivers/net/tun.c).
//
//   - pre-fix: a 4028-byte IP/UDP packet > 1514 -> TunPacket::new returns None ->
//     tx_queue.push drops it -> the fd never becomes readable -> poll() times out,
//     or (if the fixed 1500 MTU fragments the datagram) read() returns a lone
//     ~1500-byte fragment whose length is not 4028 -> RED.
//   - post-fix: with MTU 9000 the datagram is emitted whole and queued ->
//     read() returns the full 4028-byte packet -> GREEN.
static void test_tun_jumbo_packet_carry(void) {
    int fd = open_tun();
    if (fd < 0) return;

    char name[IFNAMSIZ] = {};
    // IFF_NO_PI: read() returns the bare IP packet with no 4-byte tun_pi header,
    // so the read length equals the emitted IP datagram length exactly.
    CHECK_RET(tun_setiff(fd, "tstunj0", IFF_TUN | IFF_NO_PI, name), 0,
              "TUNSETIFF IFF_TUN|IFF_NO_PI on tstunj0 succeeds");

    int csock = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(csock >= 0, "control socket for jumbo-carry test");
    if (csock < 0) { close(fd); return; }

    /* SIOCSIFADDR: 10.69.0.1/24 — also installs the connected route */
    struct ifreq ifr_a = {};
    set_ifname(ifr_a.ifr_name, name);
    struct sockaddr_in *sa = (struct sockaddr_in *)&ifr_a.ifr_addr;
    sa->sin_family = AF_INET;
    sa->sin_addr.s_addr = htonl(0x0A450001u); /* 10.69.0.1 */
    CHECK_RET(ioctl(csock, SIOCSIFADDR, &ifr_a), 0,
              "SIOCSIFADDR 10.69.0.1/24 on tstunj0");

    /* Raise the MTU to a jumbo value so the stack emits ONE unfragmented datagram.
       Without this, a >1500 payload fragments into <=1500 pieces that each fit the
       old 1514 capacity, hiding the regression. */
    struct ifreq ifr_m = {};
    set_ifname(ifr_m.ifr_name, name);
    ifr_m.ifr_mtu = 9000;
    CHECK_RET(ioctl(csock, SIOCSIFMTU, &ifr_m), 0,
              "SIOCSIFMTU 9000 (jumbo) on tstunj0");

    /* Bring the interface UP so the route is eligible */
    struct ifreq ifr_f = {};
    set_ifname(ifr_f.ifr_name, name);
    ifr_f.ifr_flags = IFF_UP;
    CHECK_RET(ioctl(csock, SIOCSIFFLAGS, &ifr_f), 0,
              "SIOCSIFFLAGS IFF_UP on tstunj0");

    int usock = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(usock >= 0, "UDP socket for jumbo sendto");
    if (usock < 0) { close(csock); close(fd); return; }

    struct sockaddr_in dst = {};
    dst.sin_family = AF_INET;
    dst.sin_addr.s_addr = htonl(0x0A450002u); /* 10.69.0.2 */
    dst.sin_port = htons(9999);
    /* 4000-byte payload -> IP/UDP packet = 20 + 8 + 4000 = 4028 B, far above the
       old fixed 1514-byte frame capacity. */
    static char payload[4000];
    memset(payload, 'J', sizeof(payload));
    ssize_t sent = sendto(usock, payload, sizeof(payload), 0,
                          (struct sockaddr *)&dst, sizeof(dst));
    CHECK(sent == (ssize_t)sizeof(payload),
          "sendto routes a 4000-B UDP datagram through tstunj0");
    if (sent != (ssize_t)sizeof(payload)) { close(usock); close(csock); close(fd); return; }

    /* Wait for the net_poll_worker to route and enqueue the packet */
    struct pollfd pfd = { .fd = fd, .events = POLLIN };
    fflush(stdout);
    int pr = poll(&pfd, 1, 3000);
    CHECK(pr == 1 && (pfd.revents & POLLIN),
          "tstunj0 fd becomes readable (jumbo packet queued, not dropped for MTU)");
    if (pr != 1 || !(pfd.revents & POLLIN)) { close(usock); close(csock); close(fd); return; }

    /* Read the whole datagram with a jumbo-sized buffer. The old fixed-1514 queue
       dropped it entirely; the heap-backed queue must return it whole. */
    static unsigned char big_buf[8192];
    memset(big_buf, 0, sizeof(big_buf));
    ssize_t n = read(fd, big_buf, sizeof(big_buf));
    CHECK(n == 4028,
          "read returns the full 4028-byte IP/UDP jumbo packet (20 IP + 8 UDP + 4000)");
    CHECK(n > 1514,
          "carried packet exceeds the old fixed 1514-byte frame capacity");

    /* The IPv4 total-length field (bytes 2..3, big-endian) must equal the read
       length, proving the datagram is intact rather than truncated in the queue. */
    if (n >= 4) {
        uint16_t ip_total = (uint16_t)((big_buf[2] << 8) | big_buf[3]);
        CHECK(ip_total == 4028,
              "IPv4 total-length header equals 4028 (packet not truncated)");
    }

    close(usock);
    close(csock);
    close(fd);
}

int main(void) {
    setbuf(stdout, NULL);
    TEST_START("tun-tap-abi");

    test_create_and_getiff();
    test_double_setiff_eexist();
    test_second_fd_ebusy();
    test_empty_name_autoalloc();
    test_persist_device_level();
    test_get_features();
    test_unsupported_flags_einval();
    test_tap_create_hwaddr();
    test_set_get_mtu();
    test_tap_mtu_upper_bound();
    test_create_close_recreate_tun();
    test_create_close_recreate_tap();
    test_template_name_alloc();
    test_tun_pi_write();
    test_tap_frame_write();
    test_tap_promiscuous_arp_reply();
    test_tap_rtnetlink_broadcast();
    test_setiff_cap_net_admin_eperm();
    test_setiff_no_strand_after_failed_bind();
    test_setiff_close_race();
    test_tun_pi_short_read();
    test_tap_pi_short_read();
    test_tun_jumbo_packet_carry();
    test_mtu_datapath_enforcement_tun_write();
    test_mtu_datapath_enforcement_tun_read();
    test_mtu_datapath_enforcement_tap();

    TEST_DONE();
}
