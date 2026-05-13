/*
 * test-futex-race.c — SMP futex TOCTOU race reproducer.
 *
 * A "blocker" thread calls FUTEX_WAIT with a matching value so it
 * actually enqueues (enters the kernel wait queue).  Two "waker"
 * threads concurrently call FUTEX_WAKE on the SAME futex address.
 * Each wake call creates a FutexGuard (via get_or_insert), finds
 * the existing table entry, wakes the blocker, and drops the guard.
 *
 * Under the OLD code (FutexGuard::drop checked strong_count outside
 * the table lock), SMP cores racing on get_or_insert/drop for the
 * same key could incorrectly remove the table entry while another
 * core's FutexGuard still referenced it — orphaning any waiter that
 * re-entered the queue.
 *
 * With the fix, strong_count is checked INSIDE the table lock,
 * making check-and-remove an atomic unit with respect to concurrent
 * get_or_insert calls.
 *
 * A watchdog thread monitors the blocker's progress; if it stalls
 * for 15+ seconds it fires and prints FAIL.
 */
#define _GNU_SOURCE
#include "test_framework.h"
#include <sys/syscall.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <sched.h>
#include <unistd.h>
#include <limits.h>
#include <stdatomic.h>

#define FUTEX_WAIT         0
#define FUTEX_WAKE         1
#define PINGPONG_ROUNDS    50
#define STACK_SIZE         (64 * 1024)

static atomic_int g_futex_val;
static volatile int g_block_count;
static volatile int g_wake_count;
static volatile int g_stalled;
static volatile int g_wakers_go;    /* start barrier for wakers */

/* ─── Blocker: actually enqueues via FUTEX_WAIT ────────────────── */

static int blocker_thread(void *arg) {
    (void)arg;

    for (int i = 0; i < PINGPONG_ROUNDS; i++) {
        /*
         * Set futex_val to 0, then call FUTEX_WAIT with val=0.
         * Because the value MATCHES, the kernel enqueues us for
         * real.  Wakers will set futex_val to 1 and call
         * FUTEX_WAKE to wake us.  If a waker already flipped the
         * value we get EAGAIN, which is harmless.
         */
        atomic_store(&g_futex_val, 0);
        g_wakers_go = 1;   /* signal wakers to start blasting */

        long rc = syscall(SYS_futex, &g_futex_val, FUTEX_WAIT,
                          0, NULL, NULL, 0);
        if (rc == -1 && errno != EAGAIN) {
            printf("  FAIL | %s:%d | blocker futex_wait errno=%d (%s)\n",
                   __FILE__, __LINE__, errno, strerror(errno));
            return 1;
        }
        /* Woken (or EAGAIN) — one ping-pong round done. */
        __atomic_fetch_add(&g_block_count, 1, __ATOMIC_RELAXED);
    }

    return 0;
}

/* ─── Waker: calls FUTEX_WAKE on the same futex ────────────────── */

static int waker_thread(void *arg) {
    int wid = (int)(long)arg;
    (void)wid;

    /* Wait for blocker to signal readiness */
    while (!g_wakers_go && !g_stalled) {
        sched_yield();
    }

    while (!g_stalled && __atomic_load_n(&g_block_count, __ATOMIC_RELAXED)
           < PINGPONG_ROUNDS) {
        atomic_store(&g_futex_val, 1);
        syscall(SYS_futex, &g_futex_val, FUTEX_WAKE,
                INT_MAX, NULL, NULL, 0);
        __atomic_fetch_add(&g_wake_count, 1, __ATOMIC_RELAXED);
        /* Let the blocker get CPU time on slow QEMU SMP. */
        usleep(100);
    }

    return 0;
}

/* ─── Watchdog ──────────────────────────────────────────────────── */

static int watchdog_thread(void *arg) {
    (void)arg;

    int last = 0, stalls = 0;

    for (int sec = 0; sec < 90; sec++) {
        sleep(1);
        int curr = __atomic_load_n(&g_block_count, __ATOMIC_RELAXED);
        if (curr >= PINGPONG_ROUNDS)
            return 0;

        if (curr == last) {
            if (++stalls >= 15) {
                atomic_store(&g_futex_val, 1);
                syscall(SYS_futex, &g_futex_val, FUTEX_WAKE,
                        INT_MAX, NULL, NULL, 0);
                sleep(1);
                int c2 = __atomic_load_n(&g_block_count, __ATOMIC_RELAXED);
                if (c2 == last) {
                    g_stalled = 1;
                    printf("  FAIL | %s:%d | stalled after %ds "
                           "(round %d/%d, wakes=%d)\n",
                           __FILE__, __LINE__, sec,
                           curr, PINGPONG_ROUNDS, g_wake_count);
                    _exit(1);
                }
                stalls = 0;
            }
        } else {
            stalls = 0;
            last = curr;
        }
    }

    if (__atomic_load_n(&g_block_count, __ATOMIC_RELAXED) < PINGPONG_ROUNDS) {
        printf("  FAIL | %s:%d | timeout 90s (round %d/%d)\n",
               __FILE__, __LINE__, g_block_count, PINGPONG_ROUNDS);
        _exit(1);
    }
    return 0;
}

/* ─── Main ──────────────────────────────────────────────────────── */

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    TEST_START("futex_race");

    {
        atomic_init(&g_futex_val, 0);
        g_block_count = 0;
        g_wake_count = 0;
        g_stalled = 0;
        g_wakers_go = 0;

        void *stk_b = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                           MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        void *stk_w1 = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                            MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        void *stk_w2 = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                            MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        void *stk_wd = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                            MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);

        CHECK(stk_b  != MAP_FAILED, "stack blocker");
        CHECK(stk_w1 != MAP_FAILED, "stack waker1");
        CHECK(stk_w2 != MAP_FAILED, "stack waker2");
        CHECK(stk_wd != MAP_FAILED, "stack watchdog");

        if (stk_b == MAP_FAILED || stk_w1 == MAP_FAILED ||
            stk_w2 == MAP_FAILED || stk_wd == MAP_FAILED)
            goto cleanup;

        int f = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND;

        int twd = clone(watchdog_thread,
                        (char *)stk_wd + STACK_SIZE, f, NULL);
        CHECK(twd >= 0, "clone watchdog");

        int tb = clone(blocker_thread,
                       (char *)stk_b + STACK_SIZE, f, NULL);
        CHECK(tb >= 0, "clone blocker");

        int tw1 = clone(waker_thread,
                        (char *)stk_w1 + STACK_SIZE, f, (void *)0);
        CHECK(tw1 >= 0, "clone waker1");

        int tw2 = clone(waker_thread,
                        (char *)stk_w2 + STACK_SIZE, f, (void *)1);
        CHECK(tw2 >= 0, "clone waker2");

        if (tb >= 0)  { int s; waitpid(tb,  &s, __WALL); }
        if (tw1 >= 0) { int s; waitpid(tw1, &s, __WALL); }
        if (tw2 >= 0) { int s; waitpid(tw2, &s, __WALL); }
        if (twd >= 0) { int s; waitpid(twd, &s, __WALL); }

        CHECK(!g_stalled, "no stall");
        CHECK(g_block_count == PINGPONG_ROUNDS, "blocker finished");
        CHECK(g_wake_count > 0, "wakers ran");

    cleanup:
        if (stk_b  != MAP_FAILED) munmap(stk_b,  STACK_SIZE);
        if (stk_w1 != MAP_FAILED) munmap(stk_w1, STACK_SIZE);
        if (stk_w2 != MAP_FAILED) munmap(stk_w2, STACK_SIZE);
        if (stk_wd != MAP_FAILED) munmap(stk_wd, STACK_SIZE);
    }

    TEST_DONE();
}
