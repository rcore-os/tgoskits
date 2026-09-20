#define _GNU_SOURCE
#include "test_framework.h"

#include <stdint.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <time.h>
#include <unistd.h>

static long monotonic_ms(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1000L + ts.tv_nsec / 1000000L;
}

static int watch(int epfd, int fd)
{
    struct epoll_event event = {
        .events = EPOLLIN,
        .data.fd = fd,
    };
    return epoll_ctl(epfd, EPOLL_CTL_ADD, fd, &event);
}

static void make_readable(int efd, const char *msg)
{
    uint64_t one = 1;
    CHECK_RET(write(efd, &one, sizeof(one)), sizeof(one), msg);
}

static void expect_outer_wakes(int outer, int inner, const char *msg)
{
    struct epoll_event event = {0};
    long start = monotonic_ms();
    int n = epoll_wait(outer, &event, 1, 1000);
    long waited = monotonic_ms() - start;
    printf("  INFO | outer epoll_wait returned %d after %ld ms\n", n, waited);
    CHECK(n == 1 && event.data.fd == inner, msg);
    CHECK(waited < 500, "outer wake is not held back until its timeout");
}

/*
 * The compositor/libinput shape: libinput waits on its own epoll on every
 * dispatch and returns without sleeping, while the compositor's main loop
 * watches that epoll fd. A waiter that has already returned must not stay
 * registered on the inner epoll where it can take the one wakeup meant for
 * the outer epoll, or input stops reaching the compositor.
 */
static void test_returned_inner_waiters_do_not_take_nested_wakeup(void)
{
    int efd = eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC);
    int inner = epoll_create1(EPOLL_CLOEXEC);
    CHECK(efd >= 0 && inner >= 0, "create eventfd and inner epoll");
    CHECK_RET(watch(inner, efd), 0, "inner epoll watches the eventfd");

    struct epoll_event event;
    for (int i = 0; i < 4; i++)
        CHECK_RET(epoll_wait(inner, &event, 1, 0), 0,
                  "idle inner wait with zero timeout returns nothing");
    for (int i = 0; i < 4; i++)
        CHECK_RET(epoll_wait(inner, &event, 1, 10), 0,
                  "idle inner wait with short timeout returns nothing");

    int outer = epoll_create1(EPOLL_CLOEXEC);
    CHECK(outer >= 0, "create outer epoll");
    CHECK_RET(watch(outer, inner), 0, "outer epoll watches the inner epoll");

    make_readable(efd, "make the inner epoll ready");
    expect_outer_wakes(outer, inner, "outer epoll reports the ready inner epoll");

    make_readable(efd, "publish again while the inner entry is still queued");
    expect_outer_wakes(outer, inner, "outer epoll still reports the inner epoll");

    close(outer);
    close(inner);
    close(efd);
}

/* Same edges without returned inner waiters: the nested wakeup must work here too. */
static void test_nested_wakeup_without_prior_inner_waits(void)
{
    int efd = eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC);
    int inner = epoll_create1(EPOLL_CLOEXEC);
    int outer = epoll_create1(EPOLL_CLOEXEC);
    CHECK(efd >= 0 && inner >= 0 && outer >= 0, "create eventfd and both epolls");
    CHECK_RET(watch(inner, efd), 0, "inner epoll watches the eventfd");
    CHECK_RET(watch(outer, inner), 0, "outer epoll watches the inner epoll");

    make_readable(efd, "make the inner epoll ready");
    expect_outer_wakes(outer, inner, "outer epoll reports the ready inner epoll");

    close(outer);
    close(inner);
    close(efd);
}

int main(void)
{
    TEST_START("nested epoll wakeup survives returned inner waiters");
    test_nested_wakeup_without_prior_inner_waits();
    test_returned_inner_waiters_do_not_take_nested_wakeup();
    TEST_DONE();
}
