/*
 * test-epoll-ctl-mod
 *
 * Exercises the EPOLL_CTL_MOD re-queue path.
 *
 * Bug: Epoll::modify swaps the Arc in the interests map but does not refresh
 * the ready_queue. If the fd already has a ready event sitting in the queue
 * as a Weak<EpollInterest> pointing at the old Arc, the upgrade fails after
 * the swap and the event is silently dropped. epoll_wait hangs until another
 * edge arrives.
 *
 * Scenario:
 *   1. epoll_create1
 *   2. pipe; EPOLL_CTL_ADD on read end with EPOLLIN
 *   3. write on write end so the read end becomes ready and the interest
 *      is queued on ready_queue
 *   4. EPOLL_CTL_MOD on the same fd with a new data payload
 *   5. epoll_wait with a short timeout must return the event carrying the
 *      new data payload, not time out
 */

#include "test_framework.h"
#include <fcntl.h>
#include <sys/epoll.h>
#include <unistd.h>

int main(void)
{
    TEST_START("epoll_ctl MOD re-queues already-ready interest");

    int ep = epoll_create1(EPOLL_CLOEXEC);
    CHECK(ep >= 0, "epoll_create1");

    int pfd[2];
    CHECK_RET(pipe(pfd), 0, "pipe");

    struct epoll_event ev;
    ev.events = EPOLLIN;
    ev.data.u64 = 0x1111;
    CHECK_RET(epoll_ctl(ep, EPOLL_CTL_ADD, pfd[0], &ev), 0, "EPOLL_CTL_ADD");

    CHECK_RET((int)write(pfd[1], "x", 1), 1, "write makes read end ready");

    ev.events = EPOLLIN;
    ev.data.u64 = 0x2222;
    CHECK_RET(epoll_ctl(ep, EPOLL_CTL_MOD, pfd[0], &ev), 0, "EPOLL_CTL_MOD");

    struct epoll_event out;
    int n = epoll_wait(ep, &out, 1, 2000);
    CHECK(n == 1, "epoll_wait returns one event after MOD");
    if (n == 1) {
        CHECK(out.events & EPOLLIN, "event is EPOLLIN");
        CHECK(out.data.u64 == 0x2222, "event carries the new data payload");
    }

    close(pfd[0]);
    close(pfd[1]);
    close(ep);

    TEST_DONE();
}
