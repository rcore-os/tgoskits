/*
 * bug-futex-wait-wake: cover the futex wait regression fixed in
 * os/StarryOS/kernel/src/task/futex.rs.
 *
 * Old behavior: FUTEX_WAIT re-checked the user futex word while holding a
 * no-IRQ spinlock, so the kernel tried to perform faultable user-memory access
 * with IRQs disabled and panicked.
 *
 * Fixed behavior: the re-check is serialized with a sleepable lock, so a
 * waiting thread can block and then be woken normally.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#ifndef FUTEX_WAIT
#define FUTEX_WAIT 0
#endif

#ifndef FUTEX_WAKE
#define FUTEX_WAKE 1
#endif

#ifndef FUTEX_PRIVATE_FLAG
#define FUTEX_PRIVATE_FLAG 128
#endif

static _Atomic uint32_t futex_word = 0;
static _Atomic int waiter_ready = 0;

static int futex_wait(uint32_t *uaddr, uint32_t expected, const struct timespec *timeout)
{
    errno = 0;
    long ret = syscall(
        SYS_futex,
        uaddr,
        FUTEX_WAIT | FUTEX_PRIVATE_FLAG,
        expected,
        timeout,
        NULL,
        0
    );
    if (ret == 0) {
        return 0;
    }
    return -errno;
}

static int futex_wake(uint32_t *uaddr, int count)
{
    errno = 0;
    long ret = syscall(
        SYS_futex,
        uaddr,
        FUTEX_WAKE | FUTEX_PRIVATE_FLAG,
        count,
        NULL,
        NULL,
        0
    );
    if (ret >= 0) {
        return (int)ret;
    }
    return -errno;
}

static void *waiter_thread(void *arg)
{
    (void)arg;

    const struct timespec timeout = {
        .tv_sec = 5,
        .tv_nsec = 0,
    };
    atomic_store_explicit(&waiter_ready, 1, memory_order_release);
    return (void *)(intptr_t)futex_wait((uint32_t *)&futex_word, 0, &timeout);
}

static int test_futex_wait_wake(void)
{
    pthread_t waiter;
    atomic_store_explicit(&futex_word, 0, memory_order_relaxed);
    atomic_store_explicit(&waiter_ready, 0, memory_order_relaxed);

    int err = pthread_create(&waiter, NULL, waiter_thread, NULL);
    if (err != 0) {
        printf("pthread_create failed: errno=%d (%s)\n", err, strerror(err));
        return 1;
    }

    while (atomic_load_explicit(&waiter_ready, memory_order_acquire) == 0) {
        sched_yield();
    }

    const struct timespec settle = {
        .tv_sec = 0,
        .tv_nsec = 50 * 1000 * 1000,
    };
    nanosleep(&settle, NULL);

    atomic_store_explicit(&futex_word, 1, memory_order_release);
    int woke = futex_wake((uint32_t *)&futex_word, 1);
    if (woke != 1) {
        printf("expected futex_wake to wake 1 waiter, got %d\n", woke);
        return 1;
    }

    void *thread_result = NULL;
    err = pthread_join(waiter, &thread_result);
    if (err != 0) {
        printf("pthread_join failed: errno=%d (%s)\n", err, strerror(err));
        return 1;
    }

    int wait_ret = (int)(intptr_t)thread_result;
    if (wait_ret != 0) {
        printf("futex wait returned %d instead of 0\n", wait_ret);
        return 1;
    }

    printf("futex waiter blocked and was woken successfully\n");
    return 0;
}

int main(void)
{
    printf("=== bug-futex-wait-wake ===\n");
    printf("Starting a waiter thread and waking it with FUTEX_WAKE...\n");

    if (test_futex_wait_wake() != 0) {
        printf("TEST FAILED\n");
        return 1;
    }

    printf("TEST PASSED\n");
    return 0;
}
