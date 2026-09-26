// TUN/TAP ABI of /dev/net/tun and the interface, address, route and rtnetlink
// requests that configure the devices, checked against Linux drivers/net/tun.c,
// net/ipv4/devinet.c and net/ipv4/fib_frontend.c.
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include "test_framework.h"
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <net/if.h>
#include <net/route.h>
#include <netinet/in.h>
#include <poll.h>
#include <pthread.h>
#include <stdint.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef TUNSETIFF
#define TUNSETIFF 0x400454cau
#endif
#ifndef TUNSETPERSIST
#define TUNSETPERSIST 0x400454cbu
#endif
#ifndef TUNGETFEATURES
#define TUNGETFEATURES 0x800454cfu
#endif
#ifndef TUNGETIFF
#define TUNGETIFF 0x800454d2u
#endif
#ifndef IFF_TUN
#define IFF_TUN 0x0001
#endif
#ifndef IFF_TAP
#define IFF_TAP 0x0002
#endif
#ifndef IFF_PERSIST
#define IFF_PERSIST 0x0800
#endif
#ifndef IFF_NO_PI
#define IFF_NO_PI 0x1000
#endif
#ifndef IFF_ONE_QUEUE
#define IFF_ONE_QUEUE 0x2000
#endif
#ifndef IFF_TUN_EXCL
#define IFF_TUN_EXCL 0x8000
#endif
#ifndef TUN_PKT_STRIP
#define TUN_PKT_STRIP 0x0001
#endif
#ifndef ARPHRD_ETHER
#define ARPHRD_ETHER 1
#endif
#ifndef ARPHRD_NONE
#define ARPHRD_NONE 0xfffe
#endif

#define TUN_PI_LEN 4
#define ARP_FRAME_LEN 42
#define UDP_HEADERS_LEN 28

/* The zero header checksum makes the stack drop the frame once it is written. */
static const unsigned char ipv4_frame[20] = {
    0x45, 0, 0, 20, 0, 0, 0, 0, 64, IPPROTO_UDP, 0, 0, 10, 99, 0, 2, 10, 99, 0, 1,
};
static const unsigned char peer_mac[6] = {0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0x02};

static int ctl = -1;

static void set_ifname(char *dst, const char *src) {
    size_t n = strnlen(src, IFNAMSIZ - 1);
    memcpy(dst, src, n);
    dst[n] = '\0';
}

/* Non-blocking, so an empty queue answers EAGAIN instead of hanging the case. */
static int open_tun(void) {
    int fd = open("/dev/net/tun", O_RDWR | O_NONBLOCK);
    CHECK(fd >= 0, "open /dev/net/tun");
    return fd;
}

static int tun_setiff(int fd, const char *name, int flags, char *out) {
    struct ifreq ifr;
    memset(&ifr, 0, sizeof(ifr));
    set_ifname(ifr.ifr_name, name);
    ifr.ifr_flags = (short)flags;
    int ret = ioctl(fd, TUNSETIFF, &ifr);
    if (ret == 0 && out)
        set_ifname(out, ifr.ifr_name);
    return ret;
}

static struct ifreq ifreq_for(const char *name) {
    struct ifreq ifr;
    memset(&ifr, 0, sizeof(ifr));
    set_ifname(ifr.ifr_name, name);
    return ifr;
}

static int get_mtu(const char *name) {
    struct ifreq ifr = ifreq_for(name);
    if (ioctl(ctl, SIOCGIFMTU, &ifr) < 0)
        return -1;
    return ifr.ifr_mtu;
}

static int set_mtu(const char *name, int mtu) {
    struct ifreq ifr = ifreq_for(name);
    ifr.ifr_mtu = mtu;
    return ioctl(ctl, SIOCSIFMTU, &ifr);
}

static int set_up(const char *name) {
    struct ifreq ifr = ifreq_for(name);
    if (ioctl(ctl, SIOCGIFFLAGS, &ifr) < 0)
        return -1;
    ifr.ifr_flags |= IFF_UP;
    return ioctl(ctl, SIOCSIFFLAGS, &ifr);
}

static in_addr_t ipv4(unsigned a, unsigned b, unsigned c, unsigned d) {
    return htonl(a << 24 | b << 16 | c << 8 | d);
}

static int set_addr(unsigned long request, const char *name, int family, in_addr_t addr) {
    struct ifreq ifr = ifreq_for(name);
    struct sockaddr_in *sin = (struct sockaddr_in *)&ifr.ifr_addr;
    sin->sin_family = (sa_family_t)family;
    sin->sin_addr.s_addr = addr;
    return ioctl(ctl, request, &ifr);
}

static int get_addr(unsigned long request, const char *name, in_addr_t *addr) {
    struct ifreq ifr = ifreq_for(name);
    if (ioctl(ctl, request, &ifr) < 0)
        return -1;
    *addr = ((struct sockaddr_in *)&ifr.ifr_addr)->sin_addr.s_addr;
    return 0;
}

static int route(unsigned long request, const char *dev, int family, in_addr_t dst, in_addr_t mask,
                 int flags) {
    struct rtentry rt;
    char name[IFNAMSIZ];
    memset(&rt, 0, sizeof(rt));
    struct sockaddr_in *sin = (struct sockaddr_in *)&rt.rt_dst;
    sin->sin_family = (sa_family_t)family;
    sin->sin_addr.s_addr = dst;
    sin = (struct sockaddr_in *)&rt.rt_genmask;
    sin->sin_family = AF_INET;
    sin->sin_addr.s_addr = mask;
    rt.rt_flags = (short)(RTF_UP | flags);
    set_ifname(name, dev);
    rt.rt_dev = name;
    return ioctl(ctl, request, &rt);
}

static int readable(int fd, int timeout_ms) {
    struct pollfd pfd = {.fd = fd, .events = POLLIN};
    return poll(&pfd, 1, timeout_ms) == 1 && (pfd.revents & POLLIN);
}

static int send_udp(int sock, in_addr_t dst, const char *payload, size_t len) {
    struct sockaddr_in addr = {.sin_family = AF_INET, .sin_port = htons(9)};
    addr.sin_addr.s_addr = dst;
    ssize_t sent = sendto(sock, payload, len, 0, (struct sockaddr *)&addr, sizeof(addr));
    return sent == (ssize_t)len ? 0 : -1;
}

/* Linux starts IPv6 autoconfiguration on an up interface; its solicitations
 * would interleave with the frames each case reads back. */
static void quiet_ipv6(void) {
    int fd = open("/proc/sys/net/ipv6/conf/default/disable_ipv6", O_WRONLY);
    if (fd >= 0) {
        (void)!write(fd, "1", 1);
        close(fd);
    }
}

static void test_attach(void) {
    int fd = open_tun();
    if (fd < 0)
        return;

    char name[IFNAMSIZ] = {0};
    CHECK_RET(tun_setiff(fd, "tstatt0", IFF_TUN | IFF_NO_PI, name), 0, "TUNSETIFF creates a TUN device");
    CHECK(strcmp(name, "tstatt0") == 0, "TUNSETIFF returns the requested name");

    struct ifreq ifr;
    memset(&ifr, 0, sizeof(ifr));
    CHECK_RET(ioctl(fd, TUNGETIFF, &ifr), 0, "TUNGETIFF on an attached file");
    CHECK(strcmp(ifr.ifr_name, "tstatt0") == 0, "TUNGETIFF reports the name");
    CHECK((unsigned short)ifr.ifr_flags == (IFF_TUN | IFF_NO_PI), "TUNGETIFF reports IFF_TUN | IFF_NO_PI");
    CHECK_ERR(tun_setiff(fd, "tstatt1", IFF_TUN | IFF_NO_PI, NULL), EEXIST,
              "a second TUNSETIFF on an attached file");

    int other = open_tun();
    if (other >= 0) {
        CHECK_ERR(tun_setiff(other, "tstatt0", IFF_TUN | IFF_TUN_EXCL | IFF_NO_PI, NULL), EBUSY,
                  "IFF_TUN_EXCL refuses an existing device");
        CHECK_ERR(tun_setiff(other, "tstatt0", IFF_TAP | IFF_NO_PI, NULL), EINVAL,
                  "attaching to a TUN device as TAP");
        CHECK_ERR(tun_setiff(other, "tstatt0", IFF_TUN | IFF_NO_PI, NULL), EBUSY,
                  "a single-queue device takes one file");
        CHECK_ERR(tun_setiff(other, "lo", IFF_TUN | IFF_NO_PI, NULL), EINVAL,
                  "attaching to a device that is not TUN");
        CHECK_ERR(tun_setiff(other, "tstatt2", IFF_NO_PI, NULL), EINVAL, "neither IFF_TUN nor IFF_TAP");
        CHECK_RET(tun_setiff(other, "tstatt2", IFF_TUN | IFF_TAP | IFF_NO_PI, NULL), 0,
                  "IFF_TUN and IFF_TAP together");
        memset(&ifr, 0, sizeof(ifr));
        CHECK_RET(ioctl(other, TUNGETIFF, &ifr), 0, "TUNGETIFF after IFF_TUN | IFF_TAP");
        CHECK((ifr.ifr_flags & (IFF_TUN | IFF_TAP)) == IFF_TUN, "IFF_TUN takes precedence over IFF_TAP");
        close(other);
    }
    close(fd);
}

static void test_unattached(void) {
    int fd = open_tun();
    if (fd < 0)
        return;

    unsigned int features = 0;
    unsigned int wanted = IFF_TUN | IFF_TAP | IFF_NO_PI | IFF_ONE_QUEUE;
    CHECK_RET(ioctl(fd, TUNGETFEATURES, &features), 0, "TUNGETFEATURES before TUNSETIFF");
    CHECK((features & wanted) == wanted, "TUNGETFEATURES lists TUN, TAP, NO_PI and ONE_QUEUE");

    /* Linux answers EBADFD; StarryOS reports EBADF. */
    struct ifreq ifr;
    char buf[64];
    memset(&ifr, 0, sizeof(ifr));
    errno = 0;
    CHECK(ioctl(fd, TUNGETIFF, &ifr) == -1 && (errno == EBADFD || errno == EBADF),
          "TUNGETIFF before TUNSETIFF fails");
    errno = 0;
    CHECK(ioctl(fd, TUNSETPERSIST, 1L) == -1 && (errno == EBADFD || errno == EBADF),
          "TUNSETPERSIST before TUNSETIFF fails");
    errno = 0;
    CHECK(read(fd, buf, sizeof(buf)) == -1 && (errno == EBADFD || errno == EBADF),
          "read before TUNSETIFF fails");
    errno = 0;
    CHECK(write(fd, ipv4_frame, sizeof(ipv4_frame)) == -1 && (errno == EBADFD || errno == EBADF),
          "write before TUNSETIFF fails");

    struct pollfd pfd = {.fd = fd, .events = POLLIN | POLLOUT};
    CHECK(poll(&pfd, 1, 0) == 1 && pfd.revents == POLLERR, "poll before TUNSETIFF reports POLLERR");
    close(fd);
}

static void test_names(void) {
    static const char *const invalid[] = {".", "..", "tst/0", "tst 0", "tst:0", "tst%d%d", "tst%s"};
    char msg[64];
    for (size_t i = 0; i < sizeof(invalid) / sizeof(invalid[0]); i++) {
        int fd = open_tun();
        if (fd < 0)
            return;
        snprintf(msg, sizeof(msg), "invalid name \"%s\"", invalid[i]);
        CHECK_ERR(tun_setiff(fd, invalid[i], IFF_TUN | IFF_NO_PI, NULL), EINVAL, msg);
        close(fd);
    }

    int tun = open_tun();
    int tap = open_tun();
    int templ = open_tun();
    int full = open_tun();
    char name[IFNAMSIZ] = {0};
    if (tun >= 0) {
        CHECK_RET(tun_setiff(tun, "", IFF_TUN | IFF_NO_PI, name), 0, "TUNSETIFF with an empty TUN name");
        CHECK(strncmp(name, "tun", 3) == 0 && name[3] >= '0' && name[3] <= '9',
              "an empty TUN name becomes tun<N>");
    }
    if (tap >= 0) {
        CHECK_RET(tun_setiff(tap, "", IFF_TAP | IFF_NO_PI, name), 0, "TUNSETIFF with an empty TAP name");
        CHECK(strncmp(name, "tap", 3) == 0 && name[3] >= '0' && name[3] <= '9',
              "an empty TAP name becomes tap<N>");
    }
    if (templ >= 0) {
        CHECK_RET(tun_setiff(templ, "tst%dx", IFF_TUN | IFF_NO_PI, name), 0, "TUNSETIFF with a %d template");
        CHECK(strcmp(name, "tst0x") == 0, "a %d template takes the first free index");
    }
    if (full >= 0) {
        struct ifreq ifr;
        memset(&ifr, 0, sizeof(ifr));
        memcpy(ifr.ifr_name, "tstlongname0123x", IFNAMSIZ);
        ifr.ifr_flags = IFF_TUN | IFF_NO_PI;
        CHECK_RET(ioctl(full, TUNSETIFF, &ifr), 0, "TUNSETIFF with a name filling IFNAMSIZ");
        CHECK(memcmp(ifr.ifr_name, "tstlongname0123", IFNAMSIZ) == 0, "the name stops at IFNAMSIZ - 1 bytes");
    }
    close(tun);
    close(tap);
    close(templ);
    close(full);
}

static void test_persist(void) {
    int fd = open_tun();
    if (fd < 0)
        return;
    CHECK_RET(tun_setiff(fd, "tstper0", IFF_TUN | IFF_NO_PI, NULL), 0, "create tstper0");
    CHECK_RET(set_mtu("tstper0", 1400), 0, "mark this tstper0 with MTU 1400");
    CHECK_RET(ioctl(fd, TUNSETPERSIST, 1L), 0, "TUNSETPERSIST 1");
    struct ifreq ifr;
    memset(&ifr, 0, sizeof(ifr));
    CHECK_RET(ioctl(fd, TUNGETIFF, &ifr), 0, "TUNGETIFF on a persistent device");
    CHECK(ifr.ifr_flags & IFF_PERSIST, "TUNGETIFF reports IFF_PERSIST");
    close(fd);
    CHECK_RET(get_mtu("tstper0"), 1400, "a persistent device outlives its file");

    fd = open_tun();
    if (fd < 0)
        return;
    CHECK_RET(tun_setiff(fd, "tstper0", IFF_TUN | IFF_NO_PI, NULL), 0, "reattach to tstper0");
    CHECK_RET(get_mtu("tstper0"), 1400, "reattaching finds the same device");
    CHECK_RET(ioctl(fd, TUNSETPERSIST, 0L), 0, "TUNSETPERSIST 0");
    close(fd);
    CHECK_ERR(get_mtu("tstper0"), ENODEV, "a cleared IFF_PERSIST removes the device on close");

    fd = open_tun();
    if (fd < 0)
        return;
    CHECK_RET(tun_setiff(fd, "tstper0", IFF_TUN | IFF_NO_PI, NULL), 0, "create tstper0 again");
    CHECK_RET(get_mtu("tstper0"), 1500, "the new device starts at MTU 1500");
    close(fd);

    fd = open_tun();
    if (fd < 0)
        return;
    CHECK_RET(tun_setiff(fd, "tstper1", IFF_TAP | IFF_NO_PI, NULL), 0, "create TAP tstper1");
    close(fd);
    CHECK_ERR(get_mtu("tstper1"), ENODEV, "a TAP device without IFF_PERSIST goes away with its file");
    fd = open_tun();
    if (fd < 0)
        return;
    CHECK_RET(tun_setiff(fd, "tstper1", IFF_TAP | IFF_NO_PI, NULL), 0, "the name of a removed TAP is free");
    close(fd);
}

static void test_mtu(void) {
    int fd = open_tun();
    if (fd < 0)
        return;
    CHECK_RET(tun_setiff(fd, "tstmtu0", IFF_TUN | IFF_NO_PI, NULL), 0, "create tstmtu0");
    CHECK_RET(get_mtu("tstmtu0"), 1500, "a TUN device starts at MTU 1500");
    CHECK_RET(set_mtu("tstmtu0", 68), 0, "SIOCSIFMTU 68");
    CHECK_RET(get_mtu("tstmtu0"), 68, "SIOCGIFMTU reads 68 back");
    CHECK_RET(set_mtu("tstmtu0", 1280), 0, "SIOCSIFMTU 1280");
    CHECK_ERR(set_mtu("tstmtu0", 67), EINVAL, "SIOCSIFMTU below 68");
    CHECK_ERR(set_mtu("tstmtu0", -1), EINVAL, "a negative MTU");
    CHECK_RET(get_mtu("tstmtu0"), 1280, "a refused MTU keeps the previous one");
    CHECK_ERR(set_mtu("tstnone0", 1280), ENODEV, "SIOCSIFMTU on a missing device");
    close(fd);
}

enum { CAP_SETUID_OK, CAP_ATTACH, CAP_CREATE, CAP_MTU, CAP_FLAGS, CAP_RESULTS };

static void test_net_admin(void) {
    int fd = open_tun();
    if (fd < 0)
        return;
    CHECK_RET(tun_setiff(fd, "tstcap0", IFF_TUN | IFF_NO_PI, NULL), 0, "create tstcap0");
    CHECK_RET(ioctl(fd, TUNSETPERSIST, 1L), 0, "keep tstcap0 for the unprivileged child");
    close(fd);

    int pipefd[2];
    if (pipe(pipefd) != 0) {
        CHECK(0, "pipe for the unprivileged child");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        int result[CAP_RESULTS] = {-1, -1, -1, -1, -1};
        close(pipefd[0]);
        if (setuid(1000) == 0) {
            result[CAP_SETUID_OK] = 0;
            int child = open("/dev/net/tun", O_RDWR | O_NONBLOCK);
            result[CAP_ATTACH] = tun_setiff(child, "tstcap0", IFF_TUN | IFF_NO_PI, NULL) == 0 ? 0 : errno;
            close(child);
            child = open("/dev/net/tun", O_RDWR | O_NONBLOCK);
            result[CAP_CREATE] = tun_setiff(child, "tstcap1", IFF_TUN | IFF_NO_PI, NULL) == 0 ? 0 : errno;
            close(child);
            result[CAP_MTU] = set_mtu("tstcap0", 1280) == 0 ? 0 : errno;
            result[CAP_FLAGS] = set_up("tstcap0") == 0 ? 0 : errno;
        }
        (void)!write(pipefd[1], result, sizeof(result));
        _exit(0);
    }
    close(pipefd[1]);
    int result[CAP_RESULTS] = {-1, -1, -1, -1, -1};
    ssize_t got = pid > 0 ? read(pipefd[0], result, sizeof(result)) : -1;
    close(pipefd[0]);
    if (pid > 0)
        waitpid(pid, NULL, 0);

    CHECK(got == (ssize_t)sizeof(result) && result[CAP_SETUID_OK] == 0, "the child runs as uid 1000");
    CHECK(result[CAP_ATTACH] == 0, "attaching to an existing device needs no CAP_NET_ADMIN");
    CHECK(result[CAP_CREATE] == EPERM, "creating a device needs CAP_NET_ADMIN");
    CHECK(result[CAP_MTU] == EPERM, "SIOCSIFMTU needs CAP_NET_ADMIN");
    CHECK(result[CAP_FLAGS] == EPERM, "SIOCSIFFLAGS needs CAP_NET_ADMIN");

    fd = open_tun();
    if (fd < 0)
        return;
    CHECK_RET(tun_setiff(fd, "tstcap0", IFF_TUN | IFF_NO_PI, NULL), 0, "the child's close left tstcap0");
    CHECK_RET(ioctl(fd, TUNSETPERSIST, 0L), 0, "release tstcap0");
    close(fd);
}

static void test_write_checks(void) {
    int fd = open_tun();
    int pi = open_tun();
    int tap = open_tun();
    char buf[64];
    if (fd >= 0) {
        CHECK_RET(tun_setiff(fd, "tstwr0", IFF_TUN | IFF_NO_PI, NULL), 0, "create tstwr0");
        struct pollfd pfd = {.fd = fd, .events = POLLIN | POLLOUT};
        CHECK_RET(poll(&pfd, 1, 0), 0, "a down TUN device is neither readable nor writable");

        unsigned char bad[sizeof(ipv4_frame)];
        memcpy(bad, ipv4_frame, sizeof(bad));
        bad[0] = 0x55;
        CHECK_ERR(write(fd, bad, sizeof(bad)), EINVAL, "a frame that is neither IPv4 nor IPv6");
        CHECK_ERR(write(fd, ipv4_frame, sizeof(ipv4_frame)), EIO, "a valid frame on a down device");
        CHECK_RET(set_up("tstwr0"), 0, "bring tstwr0 up");
        pfd.revents = 0;
        CHECK(poll(&pfd, 1, 0) == 1 && (pfd.revents & (POLLIN | POLLOUT)) == POLLOUT,
              "an up device with an empty queue is writable only");
        CHECK_RET(write(fd, ipv4_frame, sizeof(ipv4_frame)), sizeof(ipv4_frame), "a valid frame on an up device");
        CHECK_ERR(read(fd, buf, sizeof(buf)), EAGAIN, "reading an empty queue");
    }
    if (pi >= 0) {
        unsigned char framed[TUN_PI_LEN + sizeof(ipv4_frame)] = {0, 0, 0x08, 0x00};
        memcpy(framed + TUN_PI_LEN, ipv4_frame, sizeof(ipv4_frame));
        CHECK_RET(tun_setiff(pi, "tstwr1", IFF_TUN, NULL), 0, "create tstwr1 with packet info");
        CHECK_ERR(write(pi, framed, TUN_PI_LEN - 1), EINVAL, "a write shorter than struct tun_pi");
        CHECK_RET(set_up("tstwr1"), 0, "bring tstwr1 up");
        CHECK_RET(write(pi, framed, sizeof(framed)), sizeof(framed), "a write with packet info");
    }
    if (tap >= 0) {
        unsigned char eth[14] = {0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0x02, 0x08, 0x06};
        CHECK_RET(tun_setiff(tap, "tstwr2", IFF_TAP | IFF_NO_PI, NULL), 0, "create TAP tstwr2");
        CHECK_ERR(write(tap, eth, sizeof(eth) - 1), EINVAL, "a TAP frame shorter than an Ethernet header");
        CHECK_ERR(write(tap, eth, sizeof(eth)), EIO, "a TAP frame on a down device");
    }
    close(fd);
    close(pi);
    close(tap);
}

static void test_tun_queue(void) {
    int fd = open_tun();
    int sock = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(sock >= 0, "open a UDP socket");
    if (fd < 0 || sock < 0) {
        close(fd);
        close(sock);
        return;
    }
    const in_addr_t net = ipv4(10, 66, 0, 0), mask = ipv4(255, 255, 255, 0), peer = ipv4(10, 66, 0, 2);
    CHECK_RET(tun_setiff(fd, "tstq0", IFF_TUN | IFF_NO_PI, NULL), 0, "create tstq0");
    CHECK_RET(set_addr(SIOCSIFADDR, "tstq0", AF_INET, ipv4(10, 66, 0, 1)), 0, "SIOCSIFADDR 10.66.0.1");
    in_addr_t addr = 1;
    CHECK(get_addr(SIOCGIFNETMASK, "tstq0", &addr) == 0 && addr == ipv4(255, 255, 255, 255),
          "a point-to-point address takes a /32 mask");
    CHECK(get_addr(SIOCGIFDSTADDR, "tstq0", &addr) == 0 && addr == ipv4(10, 66, 0, 1),
          "SIOCGIFDSTADDR reports the local address as the peer");
    CHECK(get_addr(SIOCGIFBRDADDR, "tstq0", &addr) == 0 && addr == 0,
          "a point-to-point address has no broadcast");
    CHECK_ERR(route(SIOCADDRT, "tstq0", AF_INET, net, mask, 0), ENETDOWN, "a route through a down device");
    CHECK_RET(set_up("tstq0"), 0, "bring tstq0 up");
    CHECK_RET(route(SIOCADDRT, "tstq0", AF_INET, net, mask, 0), 0, "SIOCADDRT 10.66.0.0/24 dev tstq0");

    unsigned char buf[2048];
    CHECK_RET(send_udp(sock, peer, "one", 3), 0, "send a datagram through tstq0");
    CHECK_RET(send_udp(sock, peer, "second", 6), 0, "send a second datagram");
    CHECK(readable(fd, 2000), "the routed datagram is queued on tstq0");
    CHECK_RET(read(fd, buf, 0), 0, "a zero-length read");
    CHECK(readable(fd, 0), "a zero-length read leaves the frame queued");
    CHECK_RET(read(fd, buf, sizeof(buf)), UDP_HEADERS_LEN + 3, "one read returns one frame");
    CHECK(buf[0] == 0x45 && buf[9] == IPPROTO_UDP && memcmp(buf + UDP_HEADERS_LEN, "one", 3) == 0,
          "the first frame carries the first datagram");
    CHECK(readable(fd, 2000), "the second datagram is queued");
    CHECK_RET(read(fd, buf, sizeof(buf)), UDP_HEADERS_LEN + 6, "the next read returns the next frame");
    CHECK(memcmp(buf + UDP_HEADERS_LEN, "second", 6) == 0, "the second frame carries the second datagram");

    CHECK_RET(send_udp(sock, peer, "truncated", 9), 0, "send a datagram for a short read");
    CHECK(readable(fd, 2000), "the datagram for the short read is queued");
    CHECK_RET(read(fd, buf, 10), 10, "a short read returns the head of the frame");
    CHECK_ERR(read(fd, buf, sizeof(buf)), EAGAIN, "the rest of a truncated frame is dropped");

    CHECK_RET(route(SIOCDELRT, "tstq0", AF_INET, net, mask, 0), 0, "SIOCDELRT 10.66.0.0/24");
    CHECK_ERR(route(SIOCDELRT, "tstq0", AF_INET, net, mask, 0), ESRCH, "deleting a missing route");
    CHECK_ERR(route(SIOCADDRT, "tstq0", AF_INET6, net, mask, 0), EAFNOSUPPORT,
              "a route whose destination is not AF_INET");
    CHECK_ERR(route(SIOCADDRT, "tstq0", AF_INET, net, ipv4(255, 0, 255, 0), 0), EINVAL,
              "a non-contiguous genmask");
    CHECK_ERR(route(SIOCADDRT, "tstq0", AF_INET, peer, mask, 0), EINVAL,
              "a destination with host bits outside the genmask");
    CHECK_ERR(route(SIOCADDRT, "tstq0", AF_INET, net, mask, RTF_GATEWAY), EINVAL,
              "RTF_GATEWAY without a gateway");
    CHECK_ERR(route(SIOCADDRT, "tstnone0", AF_INET, net, mask, 0), ENODEV, "a route through a missing device");
    close(sock);
    close(fd);
}

static void test_tun_packet_info(void) {
    int fd = open_tun();
    int sock = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(sock >= 0, "open a UDP socket");
    if (fd < 0 || sock < 0) {
        close(fd);
        close(sock);
        return;
    }
    const in_addr_t peer = ipv4(10, 67, 0, 2);
    CHECK_RET(tun_setiff(fd, "tstpi0", IFF_TUN, NULL), 0, "create tstpi0 with packet info");
    CHECK_RET(set_addr(SIOCSIFADDR, "tstpi0", AF_INET, ipv4(10, 67, 0, 1)), 0, "SIOCSIFADDR 10.67.0.1");
    CHECK_RET(set_up("tstpi0"), 0, "bring tstpi0 up");
    CHECK_RET(route(SIOCADDRT, "tstpi0", AF_INET, ipv4(10, 67, 0, 0), ipv4(255, 255, 255, 0), 0), 0,
              "SIOCADDRT 10.67.0.0/24 dev tstpi0");

    char payload[50];
    unsigned char buf[2048];
    uint16_t flags;
    memset(payload, 'p', sizeof(payload));

    CHECK_RET(send_udp(sock, peer, payload, sizeof(payload)), 0, "send a datagram through tstpi0");
    CHECK(readable(fd, 2000), "the datagram is queued on tstpi0");
    CHECK_ERR(read(fd, buf, TUN_PI_LEN - 1), EINVAL, "a read shorter than struct tun_pi");
    CHECK_ERR(read(fd, buf, sizeof(buf)), EAGAIN, "the refused read consumed the frame");

    CHECK_RET(send_udp(sock, peer, payload, sizeof(payload)), 0, "send a datagram for a short read");
    CHECK(readable(fd, 2000), "the datagram for the short read is queued");
    CHECK_RET(read(fd, buf, TUN_PI_LEN + 15), TUN_PI_LEN + 15, "a short read with packet info");
    memcpy(&flags, buf, sizeof(flags));
    CHECK(flags == TUN_PKT_STRIP && buf[2] == 0x08 && buf[3] == 0x00,
          "a truncated frame carries TUN_PKT_STRIP and ETH_P_IP");

    CHECK_RET(send_udp(sock, peer, payload, sizeof(payload)), 0, "send a datagram for a whole read");
    CHECK(readable(fd, 2000), "the datagram for the whole read is queued");
    CHECK_RET(read(fd, buf, sizeof(buf)), TUN_PI_LEN + UDP_HEADERS_LEN + sizeof(payload),
              "a whole read returns packet info and the frame");
    memcpy(&flags, buf, sizeof(flags));
    CHECK(flags == 0 && buf[2] == 0x08 && buf[3] == 0x00 && buf[TUN_PI_LEN] == 0x45,
          "a whole frame carries clean packet info");
    close(sock);
    close(fd);
}

static void arp_request(unsigned char *frame, const unsigned char *dst, const unsigned char *target_ip) {
    static const unsigned char peer_ip[4] = {192, 168, 77, 2};
    memset(frame, 0, ARP_FRAME_LEN);
    memcpy(frame, dst, 6);
    memcpy(frame + 6, peer_mac, 6);
    frame[12] = 0x08;
    frame[13] = 0x06;
    frame[15] = 1;
    frame[16] = 0x08;
    frame[18] = 6;
    frame[19] = 4;
    frame[21] = 1;
    memcpy(frame + 22, peer_mac, 6);
    memcpy(frame + 28, peer_ip, 4);
    memcpy(frame + 38, target_ip, 4);
}

static int read_arp_reply(int fd, const unsigned char *mac, const unsigned char *ip) {
    unsigned char reply[128];
    if (!readable(fd, 2000))
        return 0;
    ssize_t n = read(fd, reply, sizeof(reply));
    return n >= ARP_FRAME_LEN && memcmp(reply, peer_mac, 6) == 0 && reply[12] == 0x08 && reply[13] == 0x06 &&
           reply[21] == 2 && memcmp(reply + 22, mac, 6) == 0 && memcmp(reply + 28, ip, 4) == 0;
}

static void test_tap_arp(void) {
    int fd = open_tun();
    if (fd < 0)
        return;
    CHECK_RET(tun_setiff(fd, "tstarp0", IFF_TAP | IFF_NO_PI, NULL), 0, "create TAP tstarp0");

    struct ifreq ifr = ifreq_for("tstarp0");
    unsigned char mac[6];
    CHECK_RET(ioctl(ctl, SIOCGIFHWADDR, &ifr), 0, "SIOCGIFHWADDR on tstarp0");
    memcpy(mac, ifr.ifr_hwaddr.sa_data, sizeof(mac));
    CHECK(ifr.ifr_hwaddr.sa_family == ARPHRD_ETHER && (mac[0] & 0x03) == 0x02,
          "a TAP device has a locally administered unicast MAC");

    in_addr_t addr = 0;
    CHECK_RET(set_addr(SIOCSIFADDR, "tstarp0", AF_INET, ipv4(192, 168, 77, 1)), 0, "SIOCSIFADDR 192.168.77.1");
    CHECK(get_addr(SIOCGIFNETMASK, "tstarp0", &addr) == 0 && addr == ipv4(255, 255, 255, 0),
          "a class C address takes a /24 mask");
    CHECK(get_addr(SIOCGIFBRDADDR, "tstarp0", &addr) == 0 && addr == ipv4(192, 168, 77, 255),
          "a broadcast device derives the broadcast address");
    CHECK_RET(set_up("tstarp0"), 0, "bring tstarp0 up");

    static const unsigned char ip[4] = {192, 168, 77, 1};
    static const unsigned char other[6] = {0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0x03};
    static const unsigned char broadcast[6] = {0xff, 0xff, 0xff, 0xff, 0xff, 0xff};
    unsigned char frame[ARP_FRAME_LEN];

    arp_request(frame, other, ip);
    CHECK_RET(write(fd, frame, sizeof(frame)), sizeof(frame), "inject an ARP request for another host");
    CHECK(!readable(fd, 500), "a frame for another unicast address is not answered");
    arp_request(frame, mac, ip);
    CHECK_RET(write(fd, frame, sizeof(frame)), sizeof(frame), "inject an ARP request to the device MAC");
    CHECK(read_arp_reply(fd, mac, ip), "an ARP request to the device MAC is answered");
    arp_request(frame, broadcast, ip);
    CHECK_RET(write(fd, frame, sizeof(frame)), sizeof(frame), "inject a broadcast ARP request");
    CHECK(read_arp_reply(fd, mac, ip), "a broadcast ARP request is answered");
    close(fd);
}

static void test_classful_addr(void) {
    int fd = open_tun();
    if (fd < 0)
        return;
    CHECK_RET(tun_setiff(fd, "tstcls0", IFF_TAP | IFF_NO_PI, NULL), 0, "create TAP tstcls0");

    in_addr_t addr = 0;
    CHECK_ERR(get_addr(SIOCGIFADDR, "tstcls0", &addr), EADDRNOTAVAIL, "SIOCGIFADDR without an address");
    CHECK_ERR(set_addr(SIOCSIFNETMASK, "tstcls0", AF_INET, ipv4(255, 255, 255, 0)), EADDRNOTAVAIL,
              "SIOCSIFNETMASK without an address");
    CHECK_ERR(set_addr(SIOCSIFADDR, "tstcls0", AF_INET6, ipv4(10, 1, 2, 3)), EINVAL,
              "SIOCSIFADDR with a family other than AF_INET");
    CHECK_ERR(set_addr(SIOCSIFADDR, "tstnone0", AF_INET, ipv4(10, 1, 2, 3)), ENODEV,
              "SIOCSIFADDR on a missing device");
    CHECK_ERR(set_addr(SIOCSIFADDR, "tstcls0", AF_INET, ipv4(224, 0, 0, 5)), EINVAL, "a class D address");

    CHECK_RET(set_addr(SIOCSIFADDR, "tstcls0", AF_INET, ipv4(10, 1, 2, 3)), 0, "SIOCSIFADDR 10.1.2.3");
    CHECK(get_addr(SIOCGIFNETMASK, "tstcls0", &addr) == 0 && addr == ipv4(255, 0, 0, 0),
          "a class A address takes a /8 mask");
    CHECK_RET(set_addr(SIOCSIFADDR, "tstcls0", AF_INET, ipv4(172, 16, 1, 1)), 0, "SIOCSIFADDR 172.16.1.1");
    CHECK(get_addr(SIOCGIFADDR, "tstcls0", &addr) == 0 && addr == ipv4(172, 16, 1, 1),
          "SIOCSIFADDR replaces the address");
    CHECK(get_addr(SIOCGIFNETMASK, "tstcls0", &addr) == 0 && addr == ipv4(255, 255, 0, 0),
          "a class B address takes a /16 mask");

    CHECK_ERR(set_addr(SIOCSIFNETMASK, "tstcls0", AF_INET, ipv4(255, 255, 0, 255)), EINVAL,
              "a non-contiguous netmask");
    CHECK_RET(set_addr(SIOCSIFNETMASK, "tstcls0", AF_INET, ipv4(255, 255, 255, 0)), 0, "SIOCSIFNETMASK /24");
    CHECK(get_addr(SIOCGIFNETMASK, "tstcls0", &addr) == 0 && addr == ipv4(255, 255, 255, 0),
          "SIOCSIFNETMASK changes the prefix");

    CHECK_RET(set_addr(SIOCSIFADDR, "tstcls0", AF_INET, 0), 0, "SIOCSIFADDR 0.0.0.0");
    CHECK_ERR(get_addr(SIOCGIFADDR, "tstcls0", &addr), EADDRNOTAVAIL, "0.0.0.0 removes the address");
    close(fd);
}

#define NLMSG_ALIGNTO_NL 4U
#define NLMSG_ALIGN_NL(len) (((len) + NLMSG_ALIGNTO_NL - 1) & ~(NLMSG_ALIGNTO_NL - 1))
#define NL_ROUTE 0
#define NL_REQUEST_DUMP 0x301
#define NL_DONE 3
#define NL_ERROR 2
#define RTM_NEWLINK_NL 16
#define RTM_GETLINK_NL 18
#define RTM_NEWADDR_NL 20
#define RTM_GETADDR_NL 22
#define IFLA_BROADCAST_NL 2
#define IFLA_IFNAME_NL 3
#define IFA_LABEL_NL 3
#define IFA_BROADCAST_NL 4

struct nlmsghdr_nl {
    uint32_t len;
    uint16_t type;
    uint16_t flags;
    uint32_t seq;
    uint32_t pid;
};
struct ifinfomsg_nl {
    unsigned char family;
    unsigned char pad;
    uint16_t type;
    int32_t index;
    uint32_t flags;
    uint32_t change;
};
struct ifaddrmsg_nl {
    unsigned char family;
    unsigned char prefixlen;
    unsigned char flags;
    unsigned char scope;
    uint32_t index;
};
struct rtattr_nl {
    uint16_t len;
    uint16_t type;
};

struct nl_entry {
    int found;
    uint16_t type;
    uint32_t flags;
    int has_broadcast;
    unsigned char broadcast[6];
};

/* Dumps links (RTM_GETLINK) or IPv4 addresses (RTM_GETADDR) and records the
 * entry named `name`. */
static int nl_lookup(uint16_t request, const char *name, struct nl_entry *entry) {
    static unsigned char buf[32768];
    int nl = socket(AF_NETLINK, SOCK_RAW, NL_ROUTE);
    if (nl < 0)
        return -1;
    struct {
        struct nlmsghdr_nl hdr;
        unsigned char family;
    } req;
    memset(&req, 0, sizeof(req));
    req.hdr.len = sizeof(req);
    req.hdr.type = request;
    req.hdr.flags = NL_REQUEST_DUMP;
    req.hdr.seq = 1;
    req.family = request == RTM_GETADDR_NL ? AF_INET : AF_UNSPEC;
    memset(entry, 0, sizeof(*entry));
    if (send(nl, &req, sizeof(req), 0) != (ssize_t)sizeof(req)) {
        close(nl);
        return -1;
    }

    size_t header = request == RTM_GETADDR_NL ? sizeof(struct ifaddrmsg_nl) : sizeof(struct ifinfomsg_nl);
    for (;;) {
        ssize_t n = recv(nl, buf, sizeof(buf), 0);
        if (n <= 0) {
            close(nl);
            return -1;
        }
        size_t off = 0;
        while (off + sizeof(struct nlmsghdr_nl) <= (size_t)n) {
            struct nlmsghdr_nl hdr;
            memcpy(&hdr, buf + off, sizeof(hdr));
            if (hdr.len < sizeof(hdr) || off + hdr.len > (size_t)n)
                break;
            if (hdr.type == NL_DONE || hdr.type == NL_ERROR) {
                close(nl);
                return hdr.type == NL_DONE ? 0 : -1;
            }
            unsigned char *body = buf + off + sizeof(hdr);
            size_t attr = NLMSG_ALIGN_NL(header);
            const char *label = NULL;
            int has_broadcast = 0;
            unsigned char broadcast[6] = {0};
            while (attr + sizeof(struct rtattr_nl) <= hdr.len - sizeof(hdr)) {
                struct rtattr_nl rta;
                memcpy(&rta, body + attr, sizeof(rta));
                if (rta.len < sizeof(rta) || attr + rta.len > hdr.len - sizeof(hdr))
                    break;
                unsigned char *data = body + attr + sizeof(rta);
                size_t data_len = rta.len - sizeof(rta);
                if (rta.type == (request == RTM_GETADDR_NL ? IFA_LABEL_NL : IFLA_IFNAME_NL))
                    label = (const char *)data;
                if (rta.type == (request == RTM_GETADDR_NL ? IFA_BROADCAST_NL : IFLA_BROADCAST_NL)) {
                    has_broadcast = 1;
                    memcpy(broadcast, data, data_len < sizeof(broadcast) ? data_len : sizeof(broadcast));
                }
                attr += NLMSG_ALIGN_NL(rta.len);
            }
            if (label && strcmp(label, name) == 0 &&
                hdr.type == (request == RTM_GETADDR_NL ? RTM_NEWADDR_NL : RTM_NEWLINK_NL)) {
                entry->found = 1;
                entry->has_broadcast = has_broadcast;
                memcpy(entry->broadcast, broadcast, sizeof(broadcast));
                if (request == RTM_GETLINK_NL) {
                    struct ifinfomsg_nl info;
                    memcpy(&info, body, sizeof(info));
                    entry->type = info.type;
                    entry->flags = info.flags;
                }
            }
            off += NLMSG_ALIGN_NL(hdr.len);
        }
    }
}

static void test_rtnetlink(void) {
    int tap = open_tun();
    int tun = open_tun();
    if (tap < 0 || tun < 0) {
        close(tap);
        close(tun);
        return;
    }
    CHECK_RET(tun_setiff(tap, "tstnl0", IFF_TAP | IFF_NO_PI, NULL), 0, "create TAP tstnl0");
    CHECK_RET(tun_setiff(tun, "tstnl1", IFF_TUN | IFF_NO_PI, NULL), 0, "create TUN tstnl1");
    CHECK_RET(set_addr(SIOCSIFADDR, "tstnl0", AF_INET, ipv4(192, 168, 78, 1)), 0, "SIOCSIFADDR on tstnl0");
    CHECK_RET(set_addr(SIOCSIFADDR, "tstnl1", AF_INET, ipv4(10, 68, 0, 1)), 0, "SIOCSIFADDR on tstnl1");

    static const unsigned char all_ones[6] = {0xff, 0xff, 0xff, 0xff, 0xff, 0xff};
    struct nl_entry entry;
    CHECK(nl_lookup(RTM_GETLINK_NL, "tstnl0", &entry) == 0 && entry.found, "RTM_GETLINK lists the TAP device");
    CHECK(entry.type == ARPHRD_ETHER && (entry.flags & IFF_BROADCAST), "a TAP link is broadcast Ethernet");
    CHECK(entry.has_broadcast && memcmp(entry.broadcast, all_ones, 6) == 0,
          "a TAP link reports the Ethernet broadcast address");
    CHECK(nl_lookup(RTM_GETLINK_NL, "tstnl1", &entry) == 0 && entry.found, "RTM_GETLINK lists the TUN device");
    CHECK(entry.type == ARPHRD_NONE && (entry.flags & (IFF_POINTOPOINT | IFF_NOARP)) == (IFF_POINTOPOINT | IFF_NOARP),
          "a TUN link is point-to-point without ARP");
    CHECK(!entry.has_broadcast, "a TUN link has no link-layer broadcast");
    CHECK(nl_lookup(RTM_GETADDR_NL, "tstnl0", &entry) == 0 && entry.found && entry.has_broadcast,
          "the TAP address carries IFA_BROADCAST");
    CHECK(nl_lookup(RTM_GETADDR_NL, "tstnl1", &entry) == 0 && entry.found && !entry.has_broadcast,
          "the point-to-point address carries no IFA_BROADCAST");
    close(tap);
    close(tun);
}

#define RACE_THREADS 4

struct race_arg {
    int fd;
    pthread_barrier_t *start;
    int ret;
    int err;
};

static void *race_setiff(void *p) {
    struct race_arg *arg = p;
    pthread_barrier_wait(arg->start);
    errno = 0;
    arg->ret = tun_setiff(arg->fd, "tstrace0", IFF_TUN | IFF_NO_PI, NULL);
    arg->err = errno;
    return NULL;
}

static void test_setiff_race(void) {
    int fd = open_tun();
    if (fd < 0)
        return;
    pthread_barrier_t start;
    pthread_t threads[RACE_THREADS];
    struct race_arg args[RACE_THREADS];
    int spawned = 0;
    pthread_barrier_init(&start, NULL, RACE_THREADS);
    for (int i = 0; i < RACE_THREADS; i++) {
        args[i] = (struct race_arg){.fd = fd, .start = &start, .ret = -2};
        if (pthread_create(&threads[i], NULL, race_setiff, &args[i]) == 0)
            spawned++;
    }
    CHECK(spawned == RACE_THREADS, "spawn the TUNSETIFF racers");
    if (spawned != RACE_THREADS) {
        /* The barrier would never open; leave the file to the process exit. */
        return;
    }
    int winners = 0, exists = 0;
    for (int i = 0; i < RACE_THREADS; i++) {
        pthread_join(threads[i], NULL);
        winners += args[i].ret == 0;
        exists += args[i].ret == -1 && args[i].err == EEXIST;
    }
    pthread_barrier_destroy(&start);
    CHECK(winners == 1 && exists == RACE_THREADS - 1,
          "concurrent TUNSETIFF on one file attaches once and answers EEXIST to the rest");
    close(fd);
    CHECK_ERR(get_mtu("tstrace0"), ENODEV, "the raced device goes away with its file");
}

static void test_tun_mtu_egress(void) {
    int fd = open_tun();
    int sock = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(sock >= 0, "open a UDP socket");
    if (fd < 0 || sock < 0) {
        close(fd);
        close(sock);
        return;
    }
    const in_addr_t peer = ipv4(10, 69, 0, 2);
    CHECK_RET(tun_setiff(fd, "tstmtu1", IFF_TUN | IFF_NO_PI, NULL), 0, "create tstmtu1");
    CHECK_RET(set_addr(SIOCSIFADDR, "tstmtu1", AF_INET, ipv4(10, 69, 0, 1)), 0, "SIOCSIFADDR 10.69.0.1");
    CHECK_RET(set_mtu("tstmtu1", 1280), 0, "SIOCSIFMTU 1280 on tstmtu1");
    CHECK_RET(set_up("tstmtu1"), 0, "bring tstmtu1 up");
    CHECK_RET(route(SIOCADDRT, "tstmtu1", AF_INET, ipv4(10, 69, 0, 0), ipv4(255, 255, 255, 0), 0), 0,
              "SIOCADDRT 10.69.0.0/24 dev tstmtu1");

    static char payload[1400];
    unsigned char buf[2048];
    ssize_t longest = 0;
    memset(payload, 'm', sizeof(payload));
    /* Linux fragments a larger datagram to the device MTU and StarryOS drops it
     * at the device, so only the frame length is common ground. */
    CHECK_RET(send_udp(sock, peer, payload, sizeof(payload)), 0, "send a datagram above the MTU");
    while (readable(fd, 500)) {
        ssize_t n = read(fd, buf, sizeof(buf));
        if (n <= 0)
            break;
        if (n > longest)
            longest = n;
    }
    CHECK(longest <= 1280, "no frame read from the device exceeds its MTU");
    CHECK_RET(send_udp(sock, peer, payload, 1000), 0, "send a datagram within the MTU");
    CHECK(readable(fd, 2000), "the datagram within the MTU is queued");
    CHECK_RET(read(fd, buf, sizeof(buf)), UDP_HEADERS_LEN + 1000, "a datagram within the MTU arrives whole");
    close(sock);
    close(fd);
}

int main(void) {
    setbuf(stdout, NULL);
    TEST_START("tun-tap-abi");
    quiet_ipv6();
    ctl = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(ctl >= 0, "open the AF_INET control socket");
    if (ctl < 0) {
        TEST_DONE();
    }

    test_attach();
    test_unattached();
    test_names();
    test_persist();
    test_mtu();
    test_net_admin();
    test_write_checks();
    test_tun_queue();
    test_tun_mtu_egress();
    test_tun_packet_info();
    test_tap_arp();
    test_classful_addr();
    test_rtnetlink();
    test_setiff_race();

    TEST_DONE();
}
