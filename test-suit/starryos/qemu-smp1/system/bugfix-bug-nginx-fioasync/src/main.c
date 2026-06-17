#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <unistd.h>

#ifndef FIOASYNC
#define FIOASYNC 0x5452
#endif

#ifndef O_ASYNC
#ifdef FASYNC
#define O_ASYNC FASYNC
#else
#define O_ASYNC 020000
#endif
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

static void note_errno_fail(const char *name, const char *call, int saved_errno)
{
    char detail[192];
    snprintf(detail, sizeof(detail), "%s failed errno=%d (%s)", call,
             saved_errno, strerror(saved_errno));
    note_fail(name, detail);
}

static void close_fd(int fd)
{
    if (fd >= 0) {
        close(fd);
    }
}

static void close_pair(int sv[2])
{
    close_fd(sv[0]);
    close_fd(sv[1]);
    sv[0] = -1;
    sv[1] = -1;
}

static int make_socketpair(int sv[2], const char *name)
{
    sv[0] = -1;
    sv[1] = -1;
    errno = 0;
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) == 0) {
        return 0;
    }

    note_errno_fail(name, "socketpair(AF_UNIX, SOCK_STREAM)", errno);
    return -1;
}

static int get_flags(int fd, int *flags, const char *name)
{
    errno = 0;
    int got = fcntl(fd, F_GETFL);
    if (got >= 0) {
        *flags = got;
        return 0;
    }

    note_errno_fail(name, "fcntl(F_GETFL)", errno);
    return -1;
}

static void expect_flag_set(int fd, int flag, const char *name)
{
    int flags = 0;
    if (get_flags(fd, &flags, name) != 0) {
        return;
    }
    if ((flags & flag) == flag) {
        note_pass(name);
        return;
    }

    char detail[160];
    snprintf(detail, sizeof(detail), "flags=0x%x missing flag 0x%x", flags, flag);
    note_fail(name, detail);
}

static void expect_flag_clear(int fd, int flag, const char *name)
{
    int flags = 0;
    if (get_flags(fd, &flags, name) != 0) {
        return;
    }
    if ((flags & flag) == 0) {
        note_pass(name);
        return;
    }

    char detail[160];
    snprintf(detail, sizeof(detail), "flags=0x%x still has flag 0x%x", flags,
             flag);
    note_fail(name, detail);
}

static void expect_ret_zero(int ret, int saved_errno, const char *name,
                            const char *call)
{
    if (ret == 0) {
        note_pass(name);
        return;
    }

    char detail[192];
    snprintf(detail, sizeof(detail), "%s ret=%d errno=%d (%s), expected success",
             call, ret, saved_errno, strerror(saved_errno));
    note_fail(name, detail);
}

static void expect_errno(int ret, int saved_errno, int expected_errno,
                         const char *name)
{
    if (ret == -1 && saved_errno == expected_errno) {
        note_pass(name);
        return;
    }

    char detail[192];
    snprintf(detail, sizeof(detail),
             "ret=%d errno=%d (%s), expected -1/%d (%s)", ret, saved_errno,
             strerror(saved_errno), expected_errno, strerror(expected_errno));
    note_fail(name, detail);
}

static void test_nginx_channel_fioasync(void)
{
    int sv[2];
    if (make_socketpair(sv, "nginx channel socketpair") != 0) {
        return;
    }

    int on = 1;
    errno = 0;
    int ret = ioctl(sv[0], FIONBIO, &on);
    expect_ret_zero(ret, errno, "nginx channel FIONBIO on",
                    "ioctl(FIONBIO)");
    expect_flag_set(sv[0], O_NONBLOCK, "FIONBIO sets O_NONBLOCK");

    errno = 0;
    ret = ioctl(sv[0], FIOASYNC, &on);
    expect_ret_zero(ret, errno, "nginx channel FIOASYNC on",
                    "ioctl(FIOASYNC on)");
    expect_flag_set(sv[0], O_ASYNC, "FIOASYNC on sets O_ASYNC");

    int off = 0;
    errno = 0;
    ret = ioctl(sv[0], FIOASYNC, &off);
    expect_ret_zero(ret, errno, "nginx channel FIOASYNC off",
                    "ioctl(FIOASYNC off)");
    expect_flag_clear(sv[0], O_ASYNC, "FIOASYNC off clears O_ASYNC");
    expect_flag_set(sv[0], O_NONBLOCK, "FIOASYNC off keeps O_NONBLOCK");

    close_pair(sv);
}

static void test_fcntl_setfl_o_async(void)
{
    int sv[2];
    if (make_socketpair(sv, "F_SETFL socketpair") != 0) {
        return;
    }

    int flags = 0;
    if (get_flags(sv[0], &flags, "F_SETFL read original flags") != 0) {
        close_pair(sv);
        return;
    }

    errno = 0;
    int ret = fcntl(sv[0], F_SETFL, flags | O_ASYNC | O_NONBLOCK);
    expect_ret_zero(ret, errno, "F_SETFL enables O_ASYNC and O_NONBLOCK",
                    "fcntl(F_SETFL)");
    expect_flag_set(sv[0], O_ASYNC, "F_SETFL state has O_ASYNC");
    expect_flag_set(sv[0], O_NONBLOCK, "F_SETFL state has O_NONBLOCK");

    int current = 0;
    if (get_flags(sv[0], &current, "F_SETFL read updated flags") != 0) {
        close_pair(sv);
        return;
    }

    errno = 0;
    ret = fcntl(sv[0], F_SETFL, current & ~O_ASYNC);
    expect_ret_zero(ret, errno, "F_SETFL clears only O_ASYNC",
                    "fcntl(F_SETFL clear O_ASYNC)");
    expect_flag_clear(sv[0], O_ASYNC, "F_SETFL clear removes O_ASYNC");
    expect_flag_set(sv[0], O_NONBLOCK, "F_SETFL clear keeps O_NONBLOCK");

    close_pair(sv);
}

static void test_nginx_adjacent_fd_ops(void)
{
    int sv[2];
    if (make_socketpair(sv, "nginx fd ops socketpair") != 0) {
        return;
    }

    int on = 1;
    errno = 0;
    int ret = ioctl(sv[0], FIOASYNC, &on);
    expect_ret_zero(ret, errno, "nginx fd ops FIOASYNC on",
                    "ioctl(FIOASYNC on)");

    errno = 0;
    ret = fcntl(sv[0], F_SETOWN, getpid());
    expect_ret_zero(ret, errno, "nginx fd ops F_SETOWN", "fcntl(F_SETOWN)");

    errno = 0;
    ret = fcntl(sv[0], F_SETFD, FD_CLOEXEC);
    expect_ret_zero(ret, errno, "nginx fd ops F_SETFD FD_CLOEXEC",
                    "fcntl(F_SETFD)");

    errno = 0;
    ret = fcntl(sv[0], F_GETFD);
    if (ret >= 0 && (ret & FD_CLOEXEC) != 0) {
        note_pass("nginx fd ops F_GETFD sees FD_CLOEXEC");
    } else {
        char detail[192];
        snprintf(detail, sizeof(detail),
                 "fcntl(F_GETFD) ret=%d errno=%d (%s), expected FD_CLOEXEC",
                 ret, errno, strerror(errno));
        note_fail("nginx fd ops F_GETFD sees FD_CLOEXEC", detail);
    }

    close_pair(sv);
}

static void test_tcp_listen_fioasync(void)
{
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) {
        note_errno_fail("tcp listen socket", "socket(AF_INET, SOCK_STREAM)",
                        errno);
        return;
    }

    int on = 1;
    errno = 0;
    if (setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &on, sizeof(on)) != 0) {
        note_errno_fail("tcp listen SO_REUSEADDR", "setsockopt(SO_REUSEADDR)",
                        errno);
        close_fd(fd);
        return;
    }
    note_pass("tcp listen SO_REUSEADDR");

    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    addr.sin_port = htons(0);

    errno = 0;
    if (bind(fd, (struct sockaddr *)&addr, sizeof(addr)) != 0) {
        note_errno_fail("tcp listen bind loopback", "bind(127.0.0.1:0)",
                        errno);
        close_fd(fd);
        return;
    }
    note_pass("tcp listen bind loopback");

    errno = 0;
    if (listen(fd, 1) != 0) {
        note_errno_fail("tcp listen listen", "listen", errno);
        close_fd(fd);
        return;
    }
    note_pass("tcp listen listen");

    errno = 0;
    int ret = ioctl(fd, FIOASYNC, &on);
    expect_ret_zero(ret, errno, "tcp listen FIOASYNC on",
                    "ioctl(FIOASYNC on)");
    expect_flag_set(fd, O_ASYNC, "tcp listen FIOASYNC sets O_ASYNC");

    int off = 0;
    errno = 0;
    ret = ioctl(fd, FIOASYNC, &off);
    expect_ret_zero(ret, errno, "tcp listen FIOASYNC off",
                    "ioctl(FIOASYNC off)");
    expect_flag_clear(fd, O_ASYNC, "tcp listen FIOASYNC clears O_ASYNC");

    close_fd(fd);
}

static void test_negative_errors(void)
{
    int sv[2];
    if (make_socketpair(sv, "negative socketpair") != 0) {
        return;
    }

    errno = 0;
    int ret = ioctl(sv[0], FIOASYNC, NULL);
    expect_errno(ret, errno, EFAULT, "FIOASYNC NULL pointer returns EFAULT");

    int on = 1;
    errno = 0;
    ret = ioctl(-1, FIOASYNC, &on);
    expect_errno(ret, errno, EBADF, "FIOASYNC invalid fd returns EBADF");

    close_pair(sv);
}

int main(void)
{
    printf("=== bug-nginx-fioasync ===\n");

    test_nginx_channel_fioasync();
    test_fcntl_setfl_o_async();
    test_nginx_adjacent_fd_ops();
    test_tcp_listen_fioasync();
    test_negative_errors();

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("TEST PASSED\n");
        printf("STARRY_GROUPED_TEST_PASSED: bug-nginx-fioasync\n");
        return 0;
    }

    printf("TEST FAILED\n");
    printf("STARRY_GROUPED_TEST_FAILED: bug-nginx-fioasync\n");
    return 1;
}
