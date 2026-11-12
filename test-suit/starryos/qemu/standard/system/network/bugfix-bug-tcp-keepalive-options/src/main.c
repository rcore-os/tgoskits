#define _GNU_SOURCE
#include <errno.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

#ifndef TCP_USER_TIMEOUT
#define TCP_USER_TIMEOUT 18
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

static void expect_set_get_int(int fd, int optname, int value, const char *name)
{
    errno = 0;
    int ret = setsockopt(fd, IPPROTO_TCP, optname, &value, sizeof(value));
    if (ret != 0) {
        char detail[160];
        snprintf(detail, sizeof(detail),
                 "setsockopt ret=%d errno=%d (%s), expected success",
                 ret, errno, strerror(errno));
        note_fail(name, detail);
        return;
    }

    int got = -1;
    socklen_t got_len = sizeof(got);
    errno = 0;
    ret = getsockopt(fd, IPPROTO_TCP, optname, &got, &got_len);
    if (ret == 0 && got == value && got_len == sizeof(got)) {
        note_pass(name);
        return;
    }

    char detail[160];
    snprintf(detail, sizeof(detail),
             "getsockopt ret=%d errno=%d (%s), got=%d len=%u, expected value=%d len=%zu",
             ret, errno, strerror(errno), got, got_len, value, sizeof(got));
    note_fail(name, detail);
}

static void expect_invalid_value(int fd, int optname, int value, const char *name)
{
    errno = 0;
    int ret = setsockopt(fd, IPPROTO_TCP, optname, &value, sizeof(value));
    int saved_errno = errno;
    if (ret == -1 && saved_errno == EINVAL) {
        note_pass(name);
        return;
    }

    char detail[160];
    snprintf(detail, sizeof(detail),
             "ret=%d errno=%d (%s), expected -1/EINVAL",
             ret, saved_errno, strerror(saved_errno));
    note_fail(name, detail);
}

static void expect_udp_rejects_tcp_keepalive(void)
{
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) {
        note_fail("create udp socket", strerror(errno));
        return;
    }

    int value = 30;
    errno = 0;
    int ret = setsockopt(fd, IPPROTO_TCP, TCP_KEEPIDLE, &value, sizeof(value));
    int saved_errno = errno;
    close(fd);

    if (ret == -1 && saved_errno == ENOPROTOOPT) {
        note_pass("UDP socket rejects TCP_KEEPIDLE with ENOPROTOOPT");
        return;
    }

    char detail[160];
    snprintf(detail, sizeof(detail),
             "ret=%d errno=%d (%s), expected -1/ENOPROTOOPT",
             ret, saved_errno, strerror(saved_errno));
    note_fail("UDP TCP_KEEPIDLE", detail);
}

int main(void)
{
    printf("=== bug-tcp-keepalive-options ===\n");

    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) {
        note_fail("create tcp socket", strerror(errno));
    } else {
        expect_set_get_int(fd, TCP_KEEPIDLE, 60, "TCP_KEEPIDLE set/get");
        expect_set_get_int(fd, TCP_KEEPINTVL, 30, "TCP_KEEPINTVL set/get");
        expect_set_get_int(fd, TCP_KEEPCNT, 5, "TCP_KEEPCNT set/get");
        expect_set_get_int(fd, TCP_USER_TIMEOUT, 30000, "TCP_USER_TIMEOUT set/get");
        expect_set_get_int(fd, TCP_USER_TIMEOUT, 0, "TCP_USER_TIMEOUT accepts zero");
        expect_invalid_value(fd, TCP_KEEPIDLE, 0, "TCP_KEEPIDLE rejects zero value");
        expect_invalid_value(fd, TCP_KEEPIDLE, 32768, "TCP_KEEPIDLE rejects values above 32767");
        expect_invalid_value(fd, TCP_KEEPINTVL, 32768, "TCP_KEEPINTVL rejects values above 32767");
        expect_invalid_value(fd, TCP_KEEPCNT, 128, "TCP_KEEPCNT rejects values above 127");
        expect_invalid_value(fd, TCP_USER_TIMEOUT, -1, "TCP_USER_TIMEOUT rejects negative value");
        close(fd);
    }
    expect_udp_rejects_tcp_keepalive();

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("ALL TESTS PASSED\n");
        return 0;
    }
    printf("SOME TESTS FAILED\n");
    return 1;
}
