/*
 * test_epoll_pwait_sigsetsize.c
 *
 * epoll_pwait 的 sigsetsize 参数在 musl libc 下为 16 字节 (musl _NSIG/8 = 128/8)，
 * glibc 为 8 字节，内核 sigset 为 8 字节。内核应接受任意 >= 内核 sigset 大小的值，
 * 只读取低 8 字节。本测试通过直接发起 syscall 绕过 libc wrapper，验证不同 sigsetsize
 * 的接受行为，并验证 sigmask 为 NULL 时 sigsetsize 不再参与校验（epoll_wait 语义）。
 */

#include "test_framework.h"
#include <sys/epoll.h>
#include <sys/syscall.h>
#include <unistd.h>
#include <fcntl.h>
#include <signal.h>

static long raw_epoll_pwait(int epfd, struct epoll_event *events, int maxevents,
                            int timeout, const sigset_t *sigmask, size_t sigsetsize)
{
    return syscall(SYS_epoll_pwait, epfd, events, maxevents, timeout, sigmask, sigsetsize);
}

int main(void)
{
    TEST_START("epoll_pwait: sigsetsize 兼容 musl/glibc");

    int epfd = epoll_create1(0);
    CHECK(epfd >= 0, "epoll_create1 ok");

    int pipefd[2];
    CHECK(pipe(pipefd) == 0, "pipe created");

    struct epoll_event ev;
    ev.events = EPOLLIN;
    ev.data.fd = pipefd[0];
    CHECK_RET(epoll_ctl(epfd, EPOLL_CTL_ADD, pipefd[0], &ev), 0, "epoll_ctl ADD");

    struct epoll_event out[4];

    /* Test 1: NULL sigmask with sigsetsize = 0 (epoll_wait semantics) */
    {
        long r = raw_epoll_pwait(epfd, out, 4, 0, NULL, 0);
        CHECK(r == 0, "NULL sigmask, size=0 accepted (no events, immediate)");
    }

    /* Test 2: NULL sigmask with sigsetsize = 16 (musl would still pass this).
     * sigmask is NULL so size should not be validated. */
    {
        long r = raw_epoll_pwait(epfd, out, 4, 0, NULL, 16);
        CHECK(r == 0, "NULL sigmask, size=16 accepted");
    }

    /* Test 3: real sigmask with glibc-style sigsetsize = 8 */
    {
        sigset_t mask;
        sigemptyset(&mask);
        long r = raw_epoll_pwait(epfd, out, 4, 0, &mask, 8);
        CHECK(r == 0, "sigmask + size=8 (glibc) accepted");
    }

    /* Test 4: real sigmask with musl-style sigsetsize = 16 */
    {
        sigset_t mask;
        sigemptyset(&mask);
        long r = raw_epoll_pwait(epfd, out, 4, 0, &mask, 16);
        CHECK(r == 0, "sigmask + size=16 (musl) accepted");
    }

    /* Test 5: sigsetsize smaller than kernel sigset should fail */
    {
        sigset_t mask;
        sigemptyset(&mask);
        long r = raw_epoll_pwait(epfd, out, 4, 0, &mask, 4);
        CHECK(r == -1 && errno == EINVAL, "sigmask + size=4 rejected with EINVAL");
    }

    close(pipefd[0]);
    close(pipefd[1]);
    close(epfd);

    TEST_DONE();
}
