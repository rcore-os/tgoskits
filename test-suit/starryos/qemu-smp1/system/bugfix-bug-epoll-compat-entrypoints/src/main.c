/*
 * bug-epoll-compat-entrypoints: x86_64 programs may use old epoll_create
 * and epoll_wait syscall numbers instead of epoll_create1/epoll_pwait.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifdef SYS_epoll_create
static int failures = 0;

static void expect_errno(long ret, int expected, const char *what)
{
    if (ret == -1 && errno == expected) {
        printf("PASS: %s errno=%d\n", what, errno);
    } else {
        printf("FAIL: %s expected errno=%d got ret=%ld errno=%d (%s)\n",
               what, expected, ret, errno, strerror(errno));
        failures++;
    }
}
#endif

int main(void)
{
#ifndef SYS_epoll_create
    printf("SKIP: old epoll syscall numbers unavailable on this arch\n");
    return 0;
#else
    int epfd = epoll_create(8);
    if (epfd < 0) {
        printf("FAIL: epoll_create(8) errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }

    errno = 0;
    expect_errno(epoll_create(0), EINVAL, "epoll_create rejects size=0");

    int pipefd[2];
    if (pipe(pipefd) != 0) {
        printf("FAIL: pipe errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }
    struct epoll_event ev = {
        .events = EPOLLIN,
        .data.fd = pipefd[0],
    };
    if (epoll_ctl(epfd, EPOLL_CTL_ADD, pipefd[0], &ev) != 0) {
        printf("FAIL: epoll_ctl add errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }
    write(pipefd[1], "x", 1);

    struct epoll_event out;
    errno = 0;
    long ret = syscall(SYS_epoll_wait, epfd, &out, 1, 0);
    if (ret == 1 && out.data.fd == pipefd[0] && (out.events & EPOLLIN)) {
        printf("PASS: old epoll_wait syscall returns ready event\n");
    } else {
        printf("FAIL: old epoll_wait ret=%ld events=%#x data=%d errno=%d (%s)\n",
               ret, out.events, out.data.fd, errno, strerror(errno));
        failures++;
    }

    errno = 0;
    expect_errno(epoll_ctl(epfd, EPOLL_CTL_ADD, epfd, &ev),
                 EINVAL, "epoll_ctl rejects adding epoll fd to itself");

    close(pipefd[0]);
    close(pipefd[1]);
    close(epfd);
    if (failures == 0) {
        printf("bug-epoll-compat-entrypoints: passed\n");
    }
    return failures == 0 ? 0 : 1;
#endif
}
