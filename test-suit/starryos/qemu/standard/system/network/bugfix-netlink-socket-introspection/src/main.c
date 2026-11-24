#define _GNU_SOURCE

#include <errno.h>
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
#ifndef SO_PROTOCOL
#define SO_PROTOCOL 38
#endif
#ifndef SO_DOMAIN
#define SO_DOMAIN 39
#endif

static int passed;
static int failed;

static void expect_socket_option(int fd, int option, int expected, const char *name)
{
    int value = 0;
    socklen_t length = sizeof(value);

    errno = 0;
    int result = getsockopt(fd, SOL_SOCKET, option, &value, &length);
    if (result == 0 && value == expected && length == sizeof(value)) {
        printf("PASS: %s\n", name);
        passed++;
        return;
    }

    printf("FAIL: %s: result=%d value=%d length=%u errno=%d (%s)\n",
           name,
           result,
           value,
           (unsigned int)length,
           errno,
           strerror(errno));
    failed++;
}

int main(void)
{
    printf("=== bugfix-netlink-socket-introspection ===\n");

    int fd = socket(AF_NETLINK, SOCK_RAW, NETLINK_KOBJECT_UEVENT);
    if (fd < 0) {
        printf("FAIL: socket(AF_NETLINK, SOCK_RAW, NETLINK_KOBJECT_UEVENT): errno=%d (%s)\n",
               errno,
               strerror(errno));
        printf("STARRY_GROUPED_TEST_FAILED: bugfix-netlink-socket-introspection\n");
        return EXIT_FAILURE;
    }
    printf("PASS: socket(AF_NETLINK, SOCK_RAW, NETLINK_KOBJECT_UEVENT)\n");
    passed++;

    expect_socket_option(fd, SO_TYPE, SOCK_RAW, "SO_TYPE reports SOCK_RAW");
    expect_socket_option(fd, SO_DOMAIN, AF_NETLINK, "SO_DOMAIN reports AF_NETLINK");
    expect_socket_option(fd,
                         SO_PROTOCOL,
                         NETLINK_KOBJECT_UEVENT,
                         "SO_PROTOCOL reports NETLINK_KOBJECT_UEVENT");

    close(fd);
    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("STARRY_NETLINK_SOCKET_INTROSPECTION_PASSED\n");
        printf("STARRY_GROUPED_TEST_PASSED: bugfix-netlink-socket-introspection\n");
        return EXIT_SUCCESS;
    }

    printf("STARRY_GROUPED_TEST_FAILED: bugfix-netlink-socket-introspection\n");
    return EXIT_FAILURE;
}
