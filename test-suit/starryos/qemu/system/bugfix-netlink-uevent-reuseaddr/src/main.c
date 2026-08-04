#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

#ifndef AF_NETLINK
#define AF_NETLINK 16
#endif
#ifndef NETLINK_KOBJECT_UEVENT
#define NETLINK_KOBJECT_UEVENT 15
#endif
#ifndef NETLINK_ROUTE
#define NETLINK_ROUTE 0
#endif

struct sockaddr_nl {
    unsigned short nl_family;
    unsigned short nl_pad;
    unsigned int nl_pid;
    unsigned int nl_groups;
};

static int passed;
static int failed;

static void expect_zero(int result, const char *name)
{
    if (result == 0) {
        printf("PASS: %s\n", name);
        passed++;
        return;
    }
    printf("FAIL: %s: result=%d errno=%d (%s)\n", name, result, errno, strerror(errno));
    failed++;
}

static void expect_reuseaddr_enabled(int fd, const char *name)
{
    int enabled = 0;
    socklen_t enabled_len = sizeof(enabled);

    errno = 0;
    int result = getsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &enabled, &enabled_len);
    if (result == 0 && enabled_len == sizeof(enabled) && enabled == 1) {
        printf("PASS: %s\n", name);
        passed++;
        return;
    }

    printf("FAIL: %s: result=%d value=%d len=%u errno=%d (%s)\n",
           name,
           result,
           enabled,
           (unsigned int)enabled_len,
           errno,
           strerror(errno));
    failed++;
}

static void check_netlink_listener(int protocol,
                                   unsigned int groups,
                                   const char *socket_name,
                                   const char *reuseaddr_name,
                                   const char *reuseaddr_get_name,
                                   const char *bind_name)
{
    errno = 0;
    int fd = socket(AF_NETLINK, SOCK_RAW | SOCK_CLOEXEC | SOCK_NONBLOCK, protocol);
    if (fd < 0) {
        printf("FAIL: %s: errno=%d (%s)\n", socket_name, errno, strerror(errno));
        failed++;
        return;
    }
    printf("PASS: %s\n", socket_name);
    passed++;

    int enabled = 1;
    errno = 0;
    expect_zero(setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &enabled, sizeof(enabled)),
                reuseaddr_name);
    expect_reuseaddr_enabled(fd, reuseaddr_get_name);

    struct sockaddr_nl address = {
        .nl_family = AF_NETLINK,
        .nl_groups = groups,
    };
    errno = 0;
    expect_zero(bind(fd, (struct sockaddr *)&address, sizeof(address)), bind_name);
    close(fd);
}

int main(void)
{
    printf("=== bugfix-netlink-uevent-reuseaddr ===\n");

    check_netlink_listener(NETLINK_ROUTE,
                           0,
                           "create route netlink control socket",
                           "set SO_REUSEADDR on route netlink socket",
                           "get SO_REUSEADDR from route netlink socket",
                           "bind route netlink socket");
    check_netlink_listener(NETLINK_KOBJECT_UEVENT,
                           1,
                           "create systemd-style uevent socket",
                           "set SO_REUSEADDR on uevent socket",
                           "get SO_REUSEADDR from uevent socket",
                           "bind uevent multicast group 1");

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("STARRY_NETLINK_UEVENT_REUSEADDR_PASSED\n");
        printf("STARRY_GROUPED_TEST_PASSED: bugfix-netlink-uevent-reuseaddr\n");
        return EXIT_SUCCESS;
    }
    printf("STARRY_GROUPED_TEST_FAILED: bugfix-netlink-uevent-reuseaddr\n");
    return EXIT_FAILURE;
}
