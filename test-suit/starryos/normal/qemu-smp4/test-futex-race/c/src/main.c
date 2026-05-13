/*
 * test-futex-race.c — SMP futex TOCTOU race reproducer.
 *
 * A "blocker" thread calls FUTEX_WAIT with matching value (0) to
 * actually enqueue in the kernel wait queue.  A "waker" thread
 * sets the futex value to 1 and calls FUTEX_WAKE.  Each round:
 * blocker stores 0 → FUTEX_WAIT(val=0) → woken or EAGAIN.
 *
 * Under the OLD code (strong_count check outside table lock),
 * the waker's FutexGuard::drop could remove the table entry while
 * the blocker still held a reference, orphaning the waiter.
 */
#define _GNU_SOURCE
#include "test_framework.h"
#include <sys/syscall.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <sched.h>
#include <unistd.h>
#include <stdatomic.h>

#define FUTEX_WAIT      0
#define FUTEX_WAKE      1
#define ROUNDS          50
#define STACK_SIZE      (64 * 1024)

static atomic_int g_futex_val;
static volatile int g_block_count;
static volatile int g_wake_count;
static volatile int g_enqueue_count; /* times blocker actually entered queue */
static volatile int g_stalled;

/* ─── Blocker ──────────────────────────────────────────────────── */

static int blocker_thread(void *arg) {
    (void)arg;

    for (int i = 0; i < ROUNDS; i++) {
        atomic_store(&g_futex_val, 0);

        long rc = syscall(SYS_futex, &g_futex_val, FUTEX_WAIT,
                          0, NULL, NULL, 0);
        if (rc == -1 && errno != EAGAIN) {
            printf("  FAIL | %s:%d | blocker errno=%d (%s)\n",
                   __FILE__, __LINE__, errno, strerror(errno));
            return 1;
        }
        if (rc == 0)
            __atomic_fetch_add(&g_enqueue_count, 1, __ATOMIC_RELAXED);

        __atomic_fetch_add(&g_block_count, 1, __ATOMIC_RELAXED);
    }

    return 0;
}

/* ─── Waker ────────────────────────────────────────────────────── */

static int waker_thread(void *arg) {
    (void)arg;

    for (int i = 0; i < ROUNDS && !g_stalled; i++) {
        atomic_store(&g_futex_val, 1);
        syscall(SYS_futex, &g_futex_val, FUTEX_WAKE, 1, NULL, NULL, 0);
        __atomic_fetch_add(&g_wake_count, 1, __ATOMIC_RELAXED);
        /* Brief delay to let blocker run on slow QEMU SMP. */
        usleep(5000);
    }

    return 0;
}

/* ─── Watchdog ──────────────────────────────────────────────────── */

static int watchdog_thread(void *arg) {
    (void)arg;

    int last = 0, stalls = 0;

    for (int sec = 0; sec < 60; sec++) {
        sleep(1);
        int curr = __atomic_load_n(&g_block_count, __ATOMIC_RELAXED);
        if (curr >= ROUNDS) return 0;

        if (curr == last) {
            if (++stalls >= 10) {
                atomic_store(&g_futex_val, 1);
                syscall(SYS_futex, &g_futex_val, FUTEX_WAKE,
                        1, NULL, NULL, 0);
                sleep(1);
                int c2 = __atomic_load_n(&g_block_count, __ATOMIC_RELAXED);
                if (c2 == last) {
                    g_stalled = 1;
                    printf("  FAIL | %s:%d | stalled (r=%d/%d w=%d)\n",
                           __FILE__, __LINE__,
                           curr, ROUNDS, g_wake_count);
                    _exit(1);
                }
                stalls = 0;
            }
        } else {
            stalls = 0;
            last = curr;
        }
    }

    printf("  FAIL | %s:%d | timeout (r=%d/%d)\n",
           __FILE__, __LINE__, g_block_count, ROUNDS);
    _exit(1);
}

/* ─── Main ──────────────────────────────────────────────────────── */

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    TEST_START("futex_race");

    {
        atomic_init(&g_futex_val, 0);
        g_block_count = 0;
        g_wake_count = 0;
        g_enqueue_count = 0;
        g_stalled = 0;

        void *stk_b = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                           MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        void *stk_w = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                           MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        void *stk_d = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                           MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(stk_b != MAP_FAILED, "stack blocker");
        CHECK(stk_w != MAP_FAILED, "stack waker");
        CHECK(stk_d != MAP_FAILED, "stack watchdog");
        if (stk_b == MAP_FAILED || stk_w == MAP_FAILED ||
            stk_d == MAP_FAILED) goto cleanup;

        int f = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND;

        int td = clone(watchdog_thread,
                       (char *)stk_d + STACK_SIZE, f, NULL);
        CHECK(td >= 0, "clone watchdog");

        /* Start blocker first so it has time to enter its first
           FUTEX_WAIT before the waker starts sending WAKE. */
        int tb = clone(blocker_thread,
                       (char *)stk_b + STACK_SIZE, f, NULL);
        CHECK(tb >= 0, "clone blocker");
        usleep(100000); /* 100ms head start */

        int tw = clone(waker_thread,
                       (char *)stk_w + STACK_SIZE, f, NULL);
        CHECK(tw >= 0, "clone waker");

        if (tb >= 0) { int s; waitpid(tb, &s, __WALL); }
        if (tw >= 0) { int s; waitpid(tw, &s, __WALL); }
        if (td >= 0) { int s; waitpid(td, &s, __WALL); }

        CHECK(!g_stalled, "no stall");
        CHECK(g_block_count == ROUNDS, "blocker finished");
        CHECK(g_enqueue_count > 0, "blocker entered queue at least once");

    cleanup:
        if (stk_b != MAP_FAILED) munmap(stk_b, STACK_SIZE);
        if (stk_w != MAP_FAILED) munmap(stk_w, STACK_SIZE);
        if (stk_d != MAP_FAILED) munmap(stk_d, STACK_SIZE);
    }

    TEST_DONE();
}
