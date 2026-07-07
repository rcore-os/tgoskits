/*
 * test-futex-arena-lock — glibc uses futex (FUTEX_WAIT/FUTEX_WAKE with
 * FUTEX_PRIVATE_FLAG) for per-arena mutexes. If StarryOS futex has bugs
 * under contention, glibc's arena locking could corrupt malloc state.
 */

#include "test_framework.h"

#include <errno.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/time.h>
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

static long futex(void *addr, int op, uint32_t val,
                  const struct timespec *timeout, void *addr2, uint32_t val3)
{
    return syscall(SYS_futex, (uint32_t *)addr, op, val, timeout, addr2, val3);
}

static _Atomic uint32_t futex_word1 = 0;
static _Atomic int waiter_awake = 0;

static void *waiter_thread(void *arg)
{
    (void)arg;
    futex_word1 = 1;
    futex(&futex_word1, FUTEX_WAIT | FUTEX_PRIVATE_FLAG, 1, NULL, NULL, 0);
    waiter_awake = 1;
    return NULL;
}

static void test_futex_single_waiter(void)
{
    TEST_START("futex: single-waiter WAIT/WAKE");

    futex_word1 = 0;
    waiter_awake = 0;

    pthread_t thr;
    int rc = pthread_create(&thr, NULL, waiter_thread, NULL);
    CHECK(rc == 0, "pthread_create succeeds");
    if (rc != 0) return;

    while (futex_word1 != 1) usleep(1000);

    futex_word1 = 2;
    long woken = futex(&futex_word1, FUTEX_WAKE | FUTEX_PRIVATE_FLAG, 1, NULL, NULL, 0);
    printf("  DIAG: FUTEX_WAKE woke %ld threads\n", woken);

    pthread_join(thr, NULL);
    CHECK(waiter_awake == 1, "waiter thread woke up");
}

static _Atomic uint32_t futex_word2 = 0;
static _Atomic int threads_ready = 0;
static _Atomic int threads_woken = 0;
#define N_WAITERS 3

static void *multi_waiter(void *arg)
{
    (void)arg;
    threads_ready++;
    futex(&futex_word2, FUTEX_WAIT | FUTEX_PRIVATE_FLAG, 0, NULL, NULL, 0);
    threads_woken++;
    return NULL;
}

static void test_futex_multi_waiter(void)
{
    TEST_START("futex: multi-waiter contention");

    futex_word2 = 0;
    threads_ready = 0;
    threads_woken = 0;

    pthread_t threads[N_WAITERS];
    for (int i = 0; i < N_WAITERS; i++) {
        CHECK(pthread_create(&threads[i], NULL, multi_waiter, NULL) == 0,
              "pthread_create waiter");
    }

    while (threads_ready < N_WAITERS) usleep(1000);

    futex_word2 = 1;
    long woken = futex(&futex_word2, FUTEX_WAKE | FUTEX_PRIVATE_FLAG,
                       N_WAITERS, NULL, NULL, 0);
    printf("  DIAG: FUTEX_WAKE(all) woke %ld of %d threads\n", woken, N_WAITERS);

    for (int i = 0; i < N_WAITERS; i++) pthread_join(threads[i], NULL);
    CHECK(threads_woken == N_WAITERS, "all waiters woke up");
}

static _Atomic uint32_t futex_word3 = 0;

static void test_futex_timeout(void)
{
    TEST_START("futex: WAIT with timeout");

    futex_word3 = 0;
    struct timespec ts = { .tv_sec = 0, .tv_nsec = 100000000 };

    errno = 0;
    long rc = futex(&futex_word3, FUTEX_WAIT | FUTEX_PRIVATE_FLAG, 0, &ts, NULL, 0);
    CHECK(rc == -1 || rc == 0, "FUTEX_WAIT with timeout doesn't crash");
    if (rc == -1) CHECK(errno == ETIMEDOUT, "FUTEX_WAIT timeout returns ETIMEDOUT");
    printf("  DIAG: FUTEX_WAIT(timeout) rc=%ld errno=%d\n", rc, errno);
}

static void test_futex_wake_no_waiters(void)
{
    TEST_START("futex: WAKE with no waiters");

    _Atomic uint32_t word = 0;
    long woken = futex(&word, FUTEX_WAKE | FUTEX_PRIVATE_FLAG, 1, NULL, NULL, 0);
    CHECK(woken == 0, "FUTEX_WAKE with no waiters returns 0");
    printf("  DIAG: FUTEX_WAKE(no waiters) = %ld\n", woken);
}

int main(void)
{
    printf("NIX_FUTEX_BEGIN\n");

    test_futex_single_waiter();
    test_futex_multi_waiter();
    test_futex_timeout();
    test_futex_wake_no_waiters();

    printf("NIX_FUTEX_PASSED\n");
    return 0;
}
