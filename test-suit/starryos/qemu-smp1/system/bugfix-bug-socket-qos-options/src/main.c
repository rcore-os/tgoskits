#define _GNU_SOURCE

#include <errno.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

#ifndef IPV6_TCLASS
#define IPV6_TCLASS 67
#endif

static int passed;
static int failed;

static void note_pass(const char *name)
{
    printf("PASS: %s\n", name);
    passed++;
}

static void note_fail(const char *name, const char *detail)
{
    printf("FAIL: %s: %s\n", name, detail);
    failed++;
}

static void expect_set_get_int(int fd, int level, int optname, int value,
                               int expected, const char *name)
{
    errno = 0;
    int ret = setsockopt(fd, level, optname, &value, sizeof(value));
    if (ret != 0) {
        char detail[160];
        snprintf(detail, sizeof(detail),
                 "setsockopt ret=%d errno=%d (%s), expected success", ret,
                 errno, strerror(errno));
        note_fail(name, detail);
        return;
    }

    int got = -1;
    socklen_t got_len = sizeof(got);
    errno = 0;
    ret = getsockopt(fd, level, optname, &got, &got_len);
    if (ret == 0 && got == expected && got_len == sizeof(got)) {
        note_pass(name);
        return;
    }

    char detail[180];
    snprintf(detail, sizeof(detail),
             "getsockopt ret=%d errno=%d (%s), got=%d len=%u, expected=%d",
             ret, errno, strerror(errno), got, (unsigned)got_len, expected);
    note_fail(name, detail);
}

static void expect_sockopt_errno(int fd, int level, int optname, int value,
                                 int expected_errno, const char *name)
{
    errno = 0;
    int ret = setsockopt(fd, level, optname, &value, sizeof(value));
    int saved_errno = errno;
    if (ret == -1 && saved_errno == expected_errno) {
        note_pass(name);
        return;
    }

    char detail[160];
    snprintf(detail, sizeof(detail),
             "ret=%d errno=%d (%s), expected -1/%d", ret, saved_errno,
             strerror(saved_errno), expected_errno);
    note_fail(name, detail);
}

static void test_ipv4_tos(void)
{
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) {
        note_fail("create AF_INET TCP socket", strerror(errno));
        return;
    }

    expect_set_get_int(fd, IPPROTO_IP, IP_TOS, 0x2e, 0x2c,
                       "IP_TOS masks user ECN bits");
    expect_set_get_int(fd, IPPROTO_IP, IP_TOS, -1, 0xfc,
                       "IP_TOS truncates negative int like Linux");
    expect_set_get_int(fd, IPPROTO_IP, IP_TOS, 256, 0,
                       "IP_TOS truncates values above 255");
    expect_sockopt_errno(fd, IPPROTO_IPV6, IPV6_TCLASS, 0x20, ENOPROTOOPT,
                         "AF_INET rejects IPV6_TCLASS");

    close(fd);
}

static void test_ipv6_tclass(void)
{
    int fd = socket(AF_INET6, SOCK_STREAM, 0);
    if (fd < 0) {
        note_fail("create AF_INET6 TCP socket", strerror(errno));
        return;
    }

    expect_set_get_int(fd, IPPROTO_IPV6, IPV6_TCLASS, 0x2e, 0x2c,
                       "IPV6_TCLASS masks user ECN bits");
    expect_set_get_int(fd, IPPROTO_IPV6, IPV6_TCLASS, -1, 0,
                       "IPV6_TCLASS -1 resets to default");
    expect_set_get_int(fd, IPPROTO_IPV6, IPV6_TCLASS, 255, 252,
                       "IPV6_TCLASS accepts max byte value");
    expect_sockopt_errno(fd, IPPROTO_IPV6, IPV6_TCLASS, 256, EINVAL,
                         "IPV6_TCLASS rejects values above 255");
    expect_sockopt_errno(fd, IPPROTO_IPV6, IPV6_TCLASS, -2, EINVAL,
                         "IPV6_TCLASS rejects values below -1");

    close(fd);
}

static void test_so_priority(void)
{
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) {
        note_fail("create SO_PRIORITY TCP socket", strerror(errno));
        return;
    }

    expect_set_get_int(fd, SOL_SOCKET, SO_PRIORITY, 0, 0,
                       "SO_PRIORITY accepts zero");
    expect_set_get_int(fd, SOL_SOCKET, SO_PRIORITY, 6, 6,
                       "SO_PRIORITY accepts unprivileged max");
    expect_sockopt_errno(fd, SOL_SOCKET, SO_PRIORITY, 7, EPERM,
                         "SO_PRIORITY rejects privileged value");
    expect_sockopt_errno(fd, SOL_SOCKET, SO_PRIORITY, -1, EPERM,
                         "SO_PRIORITY rejects negative value");

    close(fd);
}

int main(void)
{
    printf("=== bug-socket-qos-options ===\n");

    test_ipv4_tos();
    test_ipv6_tclass();
    test_so_priority();

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("STARRY_GROUPED_TEST_PASSED: bug-socket-qos-options\n");
        return EXIT_SUCCESS;
    }
    printf("STARRY_GROUPED_TEST_FAILED: bug-socket-qos-options\n");
    return EXIT_FAILURE;
}
