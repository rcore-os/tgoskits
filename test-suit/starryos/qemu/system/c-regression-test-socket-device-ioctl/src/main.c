// Exercises the family-agnostic device ioctls that Linux routes through
// sock_ioctl -> dev_ioctl (net/socket.c, net/core/dev_ioctl.c): SIOCGIFNAME as
// the inverse of SIOCGIFINDEX, and SIOCGIFSLAVE (no bonding master -> EINVAL for
// a resolved interface, ENODEV for an unknown one). The same requests must be
// answered on AF_INET, AF_NETLINK and AF_PACKET sockets - musl's
// if_indextoname(3), which interface-enumeration code (e.g. dnsmasq) relies on,
// issues SIOCGIFNAME and previously failed with ENOTTY.
#define _GNU_SOURCE

#include <errno.h>
#include <net/if.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <unistd.h>

#ifndef SIOCGIFINDEX
#define SIOCGIFINDEX 0x8933
#endif
#ifndef SIOCGIFNAME
#define SIOCGIFNAME 0x8910
#endif
#ifndef SIOCGIFSLAVE
#define SIOCGIFSLAVE 0x8929
#endif
#ifndef AF_NETLINK
#define AF_NETLINK 16
#endif
#ifndef AF_PACKET
#define AF_PACKET 17
#endif
#ifndef NETLINK_ROUTE
#define NETLINK_ROUTE 0
#endif

static int passed;
static int failed;

static void check(int cond, const char *msg)
{
    if (cond) {
        printf("[ ok ] %s\n", msg);
        passed++;
    } else {
        printf("[FAIL] %s\n", msg);
        failed++;
    }
}

// Run the shared device-ioctl assertions on one socket family.
static void test_family(int domain, int type, int protocol, const char *fam)
{
    char msg[96];

    // Creating the socket is a required assertion. This system case runs as root
    // (CAP_NET_RAW is available), so every family here - including AF_PACKET, the
    // dispatch entry this test exists to exercise - is expected to open. A
    // creation failure is a hard failure, not a silent skip: otherwise a broken
    // or absent per-family device_ioctl route would let the whole case pass
    // without ever reaching the ioctl assertions below.
    int fd = socket(domain, type, protocol);
    snprintf(msg, sizeof(msg), "%s: socket() created", fam);
    check(fd >= 0, msg);
    if (fd < 0) {
        printf("       %s: socket() errno=%d (dispatch entry not exercised)\n", fam, errno);
        return;
    }

    struct ifreq ifr;

    memset(&ifr, 0, sizeof(ifr));
    strncpy(ifr.ifr_name, "lo", IFNAMSIZ - 1);
    int r = ioctl(fd, SIOCGIFINDEX, &ifr);
    snprintf(msg, sizeof(msg), "%s: SIOCGIFINDEX(lo) succeeds", fam);
    check(r == 0, msg);
    int lo_index = ifr.ifr_ifindex;
    snprintf(msg, sizeof(msg), "%s: lo has a positive ifindex", fam);
    check(r == 0 && lo_index > 0, msg);

    if (r == 0 && lo_index > 0) {
        struct ifreq back;
        memset(&back, 0, sizeof(back));
        back.ifr_ifindex = lo_index;
        r = ioctl(fd, SIOCGIFNAME, &back);
        snprintf(msg, sizeof(msg), "%s: SIOCGIFNAME(lo index) succeeds", fam);
        check(r == 0, msg);
        snprintf(msg, sizeof(msg), "%s: SIOCGIFNAME resolves index back to \"lo\"", fam);
        check(r == 0 && strcmp(back.ifr_name, "lo") == 0, msg);
    }

    struct ifreq bogus;
    memset(&bogus, 0, sizeof(bogus));
    bogus.ifr_ifindex = 999999;
    errno = 0;
    r = ioctl(fd, SIOCGIFNAME, &bogus);
    snprintf(msg, sizeof(msg), "%s: SIOCGIFNAME(unknown index) -> ENODEV", fam);
    check(r == -1 && errno == ENODEV, msg);

    // A bad user pointer must map to EFAULT, not a crash or a wrong errno.
    // Linux net/socket.c dev_ifname copies the ifreq in from user space before
    // any lookup, so an unreadable argument fails with EFAULT - the directly
    // observable ABI of the newly routed request across all three families.
    errno = 0;
    r = ioctl(fd, SIOCGIFNAME, (struct ifreq *)0);
    snprintf(msg, sizeof(msg), "%s: SIOCGIFNAME(NULL ifreq) -> EFAULT", fam);
    check(r == -1 && errno == EFAULT, msg);

    memset(&ifr, 0, sizeof(ifr));
    strncpy(ifr.ifr_name, "lo", IFNAMSIZ - 1);
    errno = 0;
    r = ioctl(fd, SIOCGIFSLAVE, &ifr);
    snprintf(msg, sizeof(msg), "%s: SIOCGIFSLAVE(lo) -> EINVAL, not ENOTTY", fam);
    check(r == -1 && errno == EINVAL, msg);

    memset(&ifr, 0, sizeof(ifr));
    strncpy(ifr.ifr_name, "nosuchif0", IFNAMSIZ - 1);
    errno = 0;
    r = ioctl(fd, SIOCGIFSLAVE, &ifr);
    snprintf(msg, sizeof(msg), "%s: SIOCGIFSLAVE(unknown) -> ENODEV", fam);
    check(r == -1 && errno == ENODEV, msg);

    close(fd);
}

int main(void)
{
    test_family(AF_INET, SOCK_DGRAM, 0, "AF_INET");
    test_family(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE, "AF_NETLINK");
    test_family(AF_PACKET, SOCK_DGRAM, 0, "AF_PACKET");

    // The realistic path: musl if_indextoname()/if_nametoindex() issue SIOCGIFNAME
    // under the hood, so a clean round-trip proves the interface-discovery use case.
    unsigned int lo = if_nametoindex("lo");
    check(lo > 0, "if_nametoindex(lo) > 0");
    char name[IFNAMSIZ];
    check(lo > 0 && if_indextoname(lo, name) != NULL && strcmp(name, "lo") == 0,
          "if_indextoname(lo) round-trips to \"lo\"");

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("TEST PASSED\n");
        printf("STARRY_GROUPED_TEST_PASSED: socket-device-ioctl\n");
        return 0;
    }
    printf("TEST FAILED\n");
    printf("STARRY_GROUPED_TEST_FAILED: socket-device-ioctl\n");
    return 1;
}
