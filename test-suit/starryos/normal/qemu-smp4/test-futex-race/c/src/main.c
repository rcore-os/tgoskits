/*
 * test-futex-race.c — SMP futex table stress test.
 *
 * Multiple threads access the SAME futex word simultaneously from
 * different cores, creating concurrent FutexGuards that share a
 * single FutexTable entry.  Each FUTEX_WAIT/Wake call creates a
 * FutexGuard (via get_or_insert) and drops it, stressing the
 * strong_count check in FutexGuard::drop.
 *
 * The fix being tested: FutexGuard::drop now checks strong_count
 * INSIDE the table lock, preventing a TOCTOU race where a concurrent
 * get_or_insert clones the Arc between the check and the remove.
 *
 * A watchdog thread monitors total ops; if progress stalls for 5+
 * seconds (orphaned entry / lost wakeup) it prints FAIL.
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

#define FUTEX_WAIT      0
#define FUTEX_WAKE      1
#define N_WORKERS       3
#define WAIT_ROUNDS     5000
#define STACK_SIZE      (64 * 1024)

/* All workers hammer on the SAME futex word to force concurrent
   get_or_insert / drop on a single FutexKey. */
static atomic_int g_futex;

/* Progress counters */
static volatile int g_ops[N_WORKERS];
static volatile int g_done;
static volatile int g_stalled;

/* ─── Worker: rapid WAIT/Wake cycles on shared futex ───────────── */

static int worker_thread(void *arg) {
    int wid = (int)(long)arg;

    for (int i = 0; i < WAIT_ROUNDS && !g_stalled; i++) {
        /* Flip between WAIT and WAKE each iteration to mix up the
           concurrent access pattern on the shared key. */
        if (i & 1) {
            /* WAKE — create FutexGuard, no-op wake, drop guard */
            long rc = syscall(SYS_futex, &g_futex, FUTEX_WAKE,
                              1, NULL, NULL, 0);
            if (rc < 0) {
                printf("  FAIL | %s:%d | w%d wake errno=%d (%s)\n",
                       __FILE__, __LINE__, wid, errno, strerror(errno));
                return 1;
            }
        } else {
            /* WAIT with non-matching value to trigger EAGAIN —
               creates FutexGuard, checks value, drops immediately */
            int cur = atomic_load(&g_futex);
            long rc = syscall(SYS_futex, &g_futex, FUTEX_WAIT,
                              cur + 1, NULL, NULL, 0);
            if (rc != -1 || errno != EAGAIN) {
                if (rc >= 0) {
                    /* Was actually woken (someone changed value to
                       match).  This is fine — unexpectedly woken. */
                    atomic_fetch_add(&g_futex, 1);
                } else if (errno != EAGAIN) {
                    printf("  FAIL | %s:%d | w%d wait errno=%d (%s)\n",
                           __FILE__, __LINE__, wid, errno, strerror(errno));
                    return 1;
                }
            }
        }
        __atomic_fetch_add(&g_ops[wid], 1, __ATOMIC_RELAXED);
    }

    return 0;
}

/* ─── Watchdog ──────────────────────────────────────────────────── */

static int watchdog_thread(void *arg) {
    (void)arg;

    int last = 0, stalls = 0;

    for (int sec = 0; sec < 90; sec++) {
        sleep(1);
        if (g_done) return 0;

        int total = 0;
        for (int w = 0; w < N_WORKERS; w++)
            total += __atomic_load_n(&g_ops[w], __ATOMIC_RELAXED);

        if (total == last) {
            if (++stalls >= 5) {
                printf("  FAIL | %s:%d | stall after %ds "
                       "(total=%d/%d ops)\n",
                       __FILE__, __LINE__, sec,
                       total, N_WORKERS * WAIT_ROUNDS);
                g_stalled = 1;
                _exit(1);
            }
        } else {
            stalls = 0;
            last = total;
        }
    }

    printf("  FAIL | %s:%d | timeout 60s\n", __FILE__, __LINE__);
    _exit(1);
}

/* ─── Main ──────────────────────────────────────────────────────── */

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    TEST_START("futex_race");

    {
        atomic_init(&g_futex, 0);
        for (int w = 0; w < N_WORKERS; w++)
            g_ops[w] = 0;
        g_done = 0;
        g_stalled = 0;

        void *stk_wdog = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                              MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(stk_wdog != MAP_FAILED, "stack_wdog");

        int f = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND;
        int tid_wdog = clone(watchdog_thread,
                             (char *)stk_wdog + STACK_SIZE, f, NULL);
        CHECK(tid_wdog >= 0, "clone watchdog");

        int tids[N_WORKERS];
        void *stacks[N_WORKERS];

        for (int w = 0; w < N_WORKERS; w++) {
            stacks[w] = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
            CHECK(stacks[w] != MAP_FAILED, "stack worker");
            tids[w] = clone(worker_thread,
                            (char *)stacks[w] + STACK_SIZE, f,
                            (void *)(long)w);
            CHECK(tids[w] >= 0, "clone worker");
        }

        for (int w = 0; w < N_WORKERS; w++) {
            if (tids[w] >= 0) {
                int st;
                waitpid(tids[w], &st, __WALL);
            }
            if (stacks[w] != MAP_FAILED)
                munmap(stacks[w], STACK_SIZE);
        }

        g_done = 1;
        {
            int st;
            waitpid(tid_wdog, &st, __WALL);
        }

        CHECK(!g_stalled, "no stall");

        int total = 0;
        for (int w = 0; w < N_WORKERS; w++)
            total += __atomic_load_n(&g_ops[w], __ATOMIC_RELAXED);
        CHECK(total == N_WORKERS * WAIT_ROUNDS, "all rounds");

        if (stk_wdog != MAP_FAILED)
            munmap(stk_wdog, STACK_SIZE);
    }

    TEST_DONE();
}
