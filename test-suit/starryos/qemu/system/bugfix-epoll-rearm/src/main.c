#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <pthread.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <unistd.h>

/*
 * An interest whose lease was never notified stays armed across waits, so the
 * waiter may skip re-registering it. These rounds check that skipping does not
 * lose a wakeup: each round polls the whole set without readiness first, then
 * makes one descriptor ready from another thread while the waiter blocks.
 */
#define FDS 64
#define ROUNDS 32

static int fds[FDS];

struct ready_arg {
    int fd;
};

static void *make_ready(void *arg)
{
    struct ready_arg *ready = arg;
    uint64_t one = 1;
    usleep(20000);
    if (write(ready->fd, &one, sizeof(one)) != (ssize_t)sizeof(one)) {
        return (void *)1;
    }
    return NULL;
}

int main(void)
{
    int ep = epoll_create1(0);
    CHECK(ep >= 0, "create the epoll");

    for (int i = 0; i < FDS; i++) {
        fds[i] = eventfd(0, EFD_NONBLOCK);
        CHECK(fds[i] >= 0, "create an eventfd");
        struct epoll_event ev;
        memset(&ev, 0, sizeof(ev));
        ev.events = EPOLLIN;
        ev.data.u32 = (unsigned)i;
        CHECK_RET(epoll_ctl(ep, EPOLL_CTL_ADD, fds[i], &ev), 0, "register the eventfd");
    }

    struct epoll_event out[8];
    CHECK_RET(epoll_wait(ep, out, 8, 0), 0, "nothing is ready before the first write");

    int woken = 0;
    for (int round = 0; round < ROUNDS; round++) {
        /* Waits that find nothing are the ones that may skip re-registration. */
        for (int i = 0; i < 3; i++) {
            CHECK_RET(epoll_wait(ep, out, 8, 0), 0, "an idle set reports no events");
        }

        int target = round % FDS;
        struct ready_arg arg = {.fd = fds[target]};
        pthread_t writer;
        CHECK_RET(pthread_create(&writer, NULL, make_ready, &arg), 0, "start the writer");

        int n = epoll_wait(ep, out, 8, 2000);
        CHECK(n >= 1, "the late readiness still wakes the waiter");
        if (n >= 1) {
            CHECK_RET((int)out[0].data.u32, target, "the woken descriptor is the one written");
            woken++;
        }
        pthread_join(writer, NULL);

        uint64_t value = 0;
        CHECK(read(fds[target], &value, sizeof(value)) == (ssize_t)sizeof(value),
              "drain the eventfd");
        CHECK_RET(epoll_wait(ep, out, 8, 0), 0, "the set is idle again after draining");
    }
    CHECK_RET(woken, ROUNDS, "every round delivered its wakeup");

    for (int i = 0; i < FDS; i++) {
        close(fds[i]);
    }
    close(ep);

    TEST_DONE();
}
