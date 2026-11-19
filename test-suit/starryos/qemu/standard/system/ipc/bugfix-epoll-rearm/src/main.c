#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <stdio.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

/*
 * An interest whose lease was never notified stays armed across waits, so the
 * waiter may skip re-registering it. These rounds check that skipping does not
 * lose a wakeup: each round polls the whole set without readiness first, then
 * makes one descriptor ready from another thread once the waiter is blocked.
 * The rounds cycle over eight descriptors, so every lease is skipped while
 * armed and registered again after it was notified. A level-triggered interest
 * is rechecked on the next scan and registered again there, so only after an
 * edge-triggered delivery does the waiter itself decide whether to register
 * again; even descriptors are edge-triggered to cover that path.
 */
#define FDS 64
#define ROUNDS 32
#define TARGETS 8
#define WAIT_MS 5000
/* A wakeup lands within milliseconds; only a lost one runs into the timeout. */
#define WOKEN_WITHIN_MS 2500
#define ASLEEP_WITHIN_MS 2000

static int fds[FDS];
static pid_t waiter;
static int waiting;

/*
 * The waiter raises `waiting` right before epoll_wait and nothing in between
 * can sleep, so once it is raised a sleeping waiter is blocked in epoll_wait.
 */
static int waiter_is_asleep(void)
{
    char path[64];
    snprintf(path, sizeof(path), "/proc/self/task/%d/status", (int)waiter);
    FILE *status = fopen(path, "r");
    if (!status) {
        return 0;
    }
    char line[128];
    char state = 0;
    while (fgets(line, sizeof(line), status)) {
        if (sscanf(line, "State: %c", &state) == 1) {
            break;
        }
    }
    fclose(status);
    return state == 'S';
}

struct ready_arg {
    int fd;
};

static void *make_ready(void *arg)
{
    struct ready_arg *ready = arg;
    while (!__atomic_load_n(&waiting, __ATOMIC_ACQUIRE)) {
        sched_yield();
    }
    for (int waited = 0; !waiter_is_asleep(); waited++) {
        if (waited == ASLEEP_WITHIN_MS) {
            return (void *)2;
        }
        usleep(1000);
    }
    uint64_t one = 1;
    if (write(ready->fd, &one, sizeof(one)) != (ssize_t)sizeof(one)) {
        return (void *)1;
    }
    return NULL;
}

static long elapsed_ms(const struct timespec *start)
{
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (now.tv_sec - start->tv_sec) * 1000 + (now.tv_nsec - start->tv_nsec) / 1000000;
}

int main(void)
{
    waiter = (pid_t)syscall(SYS_gettid);
    int ep = epoll_create1(0);
    CHECK(ep >= 0, "create the epoll");

    for (int i = 0; i < FDS; i++) {
        fds[i] = eventfd(0, EFD_NONBLOCK);
        CHECK(fds[i] >= 0, "create an eventfd");
        struct epoll_event ev;
        memset(&ev, 0, sizeof(ev));
        ev.events = EPOLLIN | (i % 2 == 0 ? EPOLLET : 0);
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

        int target = round % TARGETS;
        struct ready_arg arg = {.fd = fds[target]};
        pthread_t writer;
        __atomic_store_n(&waiting, 0, __ATOMIC_RELAXED);
        CHECK_RET(pthread_create(&writer, NULL, make_ready, &arg), 0, "start the writer");

        struct timespec start;
        clock_gettime(CLOCK_MONOTONIC, &start);
        __atomic_store_n(&waiting, 1, __ATOMIC_RELEASE);
        int n = epoll_wait(ep, out, 8, WAIT_MS);
        long waited = elapsed_ms(&start);
        CHECK(n >= 1, "readiness made while the waiter blocks is reported");
        CHECK(waited < WOKEN_WITHIN_MS, "the write woke the waiter before its timeout");
        if (n >= 1) {
            CHECK_RET((int)out[0].data.u32, target, "the woken descriptor is the one written");
            woken++;
        }
        void *result = NULL;
        pthread_join(writer, &result);
        CHECK(result == NULL, "the writer saw the waiter blocked before writing");

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
