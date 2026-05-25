/*
 * test_epoll_pwait_sigsetsize.c
 *
 * 验证 raw epoll_pwait 的 sigsetsize 语义：
 * - sigmask == NULL 时，sigsetsize 不参与校验；
 * - sigmask != NULL 时，sigsetsize 必须严格等于 8。
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
    TEST_START("epoll_pwait: sigsetsize Linux semantics");

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

    /* Test 4: real sigmask with sigsetsize = 16 should fail */
    {
        sigset_t mask;
        sigemptyset(&mask);
        long r = raw_epoll_pwait(epfd, out, 4, 0, &mask, 16);
        CHECK(r == -1 && errno == EINVAL, "sigmask + size=16 rejected with EINVAL");
    }

    /* Test 5: non-NULL sigmask with size=0 should fail */
    {
        sigset_t mask;
        sigemptyset(&mask);
        long r = raw_epoll_pwait(epfd, out, 4, 0, &mask, 0);
        CHECK(r == -1 && errno == EINVAL, "sigmask + size=0 rejected with EINVAL");
    }

    /* Test 6: sigsetsize smaller than kernel sigset should fail */
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
