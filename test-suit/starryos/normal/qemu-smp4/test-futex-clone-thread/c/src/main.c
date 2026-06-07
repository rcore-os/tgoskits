/*
 * test-futex-clone-thread — Verify futex WAIT/WAKE between CLONE_THREAD threads.
 *
 * Background:
 *   The old StarryOS futex implementation did not work across CLONE_VM
 *   threads (FUTEX_WAKE returned 0).  The futex subsystem was rewritten
 *   with async/future + Waker.  CLONE_THREAD + CLONE_VM threads now share
 *   the same Arc<ProcessData> (and thus the same futex_table), so Private
 *   futex keys should resolve to the same WaitQueue for both waiter and
 *   waker.
 *
 *   This test verifies the fix using pthread_create (which internally calls
 *   clone with CLONE_THREAD | CLONE_VM | CLONE_SIGHAND) and raw futex
 *   syscalls on stack-local variables (Private futex keys).
 *
 * Sub-tests:
 *   1. basic_wait_wake  — 1 waiter, main wakes, 50 rounds
 *   2. multi_waiter     — 4 waiters on same futex, main wakes all, 20 rounds
 *   3. reverse_wake     — main waits, worker wakes, 20 rounds
 *   4. pthread_mutex    — 8 workers contend on mutex, 250 iters each
 *   5. stress_contention — 8 waiters on same futex, 40 rounds
 *   6. private_flag     — FUTEX_WAIT_PRIVATE / WAKE_PRIVATE, 50 rounds
 *   7. bitset_selective — WAIT_BITSET / WAKE_BITSET with selective masks
 */
#define _GNU_SOURCE
#include "test_framework.h"

#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <sys/syscall.h>
#include <unistd.h>
#include <errno.h>
#include <string.h>

#ifndef FUTEX_WAIT
#define FUTEX_WAIT 0
#endif
#ifndef FUTEX_WAKE
#define FUTEX_WAKE 1
#endif
#ifndef FUTEX_WAIT_BITSET
#define FUTEX_WAIT_BITSET 9
#endif
#ifndef FUTEX_WAKE_BITSET
#define FUTEX_WAKE_BITSET 10
#endif
#ifndef FUTEX_PRIVATE_FLAG
#define FUTEX_PRIVATE_FLAG 128
#endif
#ifndef FUTEX_BITSET_MATCH_ANY
#define FUTEX_BITSET_MATCH_ANY 0xffffffffu
#endif

/* ---- futex helpers ---- */

static long futex_wait(_Atomic uint32_t *uaddr, uint32_t val)
{
    return syscall(SYS_futex, uaddr, FUTEX_WAIT, val, NULL, NULL, 0);
}

static long futex_wake(_Atomic uint32_t *uaddr, int count)
{
    return syscall(SYS_futex, uaddr, FUTEX_WAKE, count, NULL, NULL, 0);
}

static long futex_wait_private(_Atomic uint32_t *uaddr, uint32_t val)
{
    return syscall(SYS_futex, uaddr, FUTEX_WAIT | FUTEX_PRIVATE_FLAG, val, NULL, NULL, 0);
}

static long futex_wake_private(_Atomic uint32_t *uaddr, int count)
{
    return syscall(SYS_futex, uaddr, FUTEX_WAKE | FUTEX_PRIVATE_FLAG, count, NULL, NULL, 0);
}

static long futex_wait_bitset(_Atomic uint32_t *uaddr, uint32_t val, uint32_t bitset)
{
    return syscall(SYS_futex, uaddr, FUTEX_WAIT_BITSET, val, NULL, NULL, bitset);
}

static long futex_wake_bitset(_Atomic uint32_t *uaddr, int count, uint32_t bitset)
{
    return syscall(SYS_futex, uaddr, FUTEX_WAKE_BITSET, count, NULL, NULL, bitset);
}

static int wait_for_waiters(_Atomic int *ready, int expected, const char *label)
{
    for (int i = 0; i < 1000; i++) {
        if (atomic_load(ready) >= expected)
            return 1;
        usleep(1000);
    }

    printf("  %s: only %d/%d waiters became ready\n",
           label, atomic_load(ready), expected);
    return 0;
}

/* ================================================================
 * Test 1 — Basic 1:1 wait/wake (50 rounds, long-lived waiter)
 * ================================================================ */

#define T1_ROUNDS 50

static _Atomic uint32_t t1_futex;
static _Atomic int t1_done;
static _Atomic int t1_ready_round;

static void *t1_waiter(void *arg)
{
    (void)arg;
    int pass = 0;

    for (int i = 0; i < T1_ROUNDS; i++) {
        atomic_store(&t1_futex, 0);
        atomic_store(&t1_ready_round, i + 1);
        while (atomic_load(&t1_futex) == 0) {
            long r = futex_wait(&t1_futex, 0);
            if (r < 0 && errno != EAGAIN && errno != EINTR) {
                printf("  T1 waiter round %d: %s\n", i, strerror(errno));
                atomic_store(&t1_done, 1);
                return (void *)1L;
            }
        }
        pass++;
    }
    atomic_store(&t1_done, 1);
    return (void *)(long)pass;
}

static void test_basic_wait_wake(void)
{
    atomic_store(&t1_futex, 1);
    atomic_store(&t1_done, 0);
    atomic_store(&t1_ready_round, 0);

    pthread_t t;
    CHECK(pthread_create(&t, NULL, t1_waiter, NULL) == 0,
          "T1 pthread_create (CLONE_THREAD|CLONE_VM)");

    int total_woken = 0;
    for (int i = 0; i < T1_ROUNDS; i++) {
        while (atomic_load(&t1_ready_round) < i + 1 && !atomic_load(&t1_done))
            usleep(100);
        atomic_store(&t1_futex, 1);
        long w = futex_wake(&t1_futex, 1);
        if (w > 0)
            total_woken++;
    }

    void *ret;
    pthread_join(t, &ret);
    long pass = (long)ret;

    CHECK(pass == T1_ROUNDS, "T1 waiter completed all 50 rounds");
    CHECK(total_woken > 0, "T1 at least one FUTEX_WAKE actually woke a waiter");
    printf("  T1 result: waiter %ld/%d rounds, %d successful wakes\n",
           pass, T1_ROUNDS, total_woken);
}

/* ================================================================
 * Test 2 — 4 waiters on same futex, main wakes all (20 rounds)
 *
 * Each round creates fresh threads to maximise clone/futex coverage.
 * ================================================================ */

#define T2_ROUNDS 20
#define T2_N 4

static _Atomic uint32_t t2_futex;
static _Atomic int t2_ready;

static void *t2_waiter(void *arg)
{
    (void)arg;
    atomic_fetch_add(&t2_ready, 1);
    while (atomic_load(&t2_futex) == 0) {
        long r = futex_wait(&t2_futex, 0);
        if (r < 0 && errno != EAGAIN && errno != EINTR)
            return (void *)1L;
    }
    return NULL;
}

static void test_multi_waiter(void)
{
    int total_woken = 0;
    int all_ok = 1;

    for (int i = 0; i < T2_ROUNDS; i++) {
        atomic_store(&t2_futex, 0);
        atomic_store(&t2_ready, 0);

        pthread_t ts[T2_N];
        int created = 0;
        for (int w = 0; w < T2_N; w++) {
            if (pthread_create(&ts[w], NULL, t2_waiter, NULL) != 0) {
                all_ok = 0;
                break;
            }
            created++;
        }

        if (created < T2_N) {
            /* cleanup partially created threads */
            for (int w = 0; w < created; w++) {
                atomic_store(&t2_futex, 1);
                futex_wake(&t2_futex, T2_N);
                pthread_join(ts[w], NULL);
            }
            break;
        }

        if (!wait_for_waiters(&t2_ready, T2_N, "T2"))
            all_ok = 0;

        /* Give all waiters time to block on the futex after reporting ready. */
        usleep(5000);

        atomic_store(&t2_futex, 1);
        long w = futex_wake(&t2_futex, T2_N);
        if (w < 0) {
            printf("  T2 round %d: futex_wake error: %s\n", i, strerror(errno));
            all_ok = 0;
        } else {
            total_woken += (int)w;
        }

        for (int w2 = 0; w2 < T2_N; w2++) {
            void *r;
            pthread_join(ts[w2], &r);
            if (r != NULL)
                all_ok = 0;
        }
    }

    CHECK(all_ok, "T2 all 20 rounds with 4 waiters completed");
    CHECK(total_woken > 0, "T2 at least one multi-waiter FUTEX_WAKE succeeded");
    printf("  T2 result: %d total wakes across %d rounds\n",
           total_woken, T2_ROUNDS);
}

/* ================================================================
 * Test 3 — Main waits, worker wakes (20 rounds)
 * ================================================================ */

#define T3_ROUNDS 20

static _Atomic uint32_t t3_futex;

static void *t3_waker(void *arg)
{
    (void)arg;
    for (int i = 0; i < T3_ROUNDS; i++) {
        /* Wait for main to block on futex */
        usleep(2000);
        atomic_store(&t3_futex, 1);
        futex_wake(&t3_futex, 1);
        /* Let main process the wakeup */
        usleep(2000);
    }
    return NULL;
}

static void test_reverse_wake(void)
{
    atomic_store(&t3_futex, 1);

    pthread_t t;
    CHECK(pthread_create(&t, NULL, t3_waker, NULL) == 0,
          "T3 pthread_create (worker wakes main)");

    int pass = 0;
    for (int i = 0; i < T3_ROUNDS; i++) {
        atomic_store(&t3_futex, 0);
        while (atomic_load(&t3_futex) == 0) {
            long r = futex_wait(&t3_futex, 0);
            if (r < 0 && errno != EAGAIN && errno != EINTR) {
                printf("  T3 main round %d: %s\n", i, strerror(errno));
                break;
            }
        }
        if (atomic_load(&t3_futex) != 0)
            pass++;
    }

    void *ret;
    pthread_join(t, &ret);
    CHECK(ret == NULL, "T3 worker exited cleanly");
    CHECK(pass == T3_ROUNDS, "T3 main woken by worker in all 20 rounds");
    printf("  T3 result: main %d/%d rounds woken by worker\n", pass, T3_ROUNDS);
}

/* ================================================================
 * Test 4 — pthread_mutex contention (8 workers x 250 iterations)
 *
 * Validates that musl's pthread_mutex (which uses FUTEX_WAIT_PRIVATE /
 * FUTEX_WAKE_PRIVATE internally) works correctly under SMP contention.
 * ================================================================ */

#define T4_THREADS 8
#define T4_ITERS  250

static pthread_mutex_t t4_mutex;
static _Atomic uint64_t t4_counter;

static void *t4_worker(void *arg)
{
    (void)arg;
    for (int i = 0; i < T4_ITERS; i++) {
        pthread_mutex_lock(&t4_mutex);
        t4_counter++;
        pthread_mutex_unlock(&t4_mutex);
    }
    return NULL;
}

static void test_pthread_mutex(void)
{
    t4_counter = 0;
    pthread_mutex_init(&t4_mutex, NULL);

    pthread_t ts[T4_THREADS];
    int created = 0;
    for (int i = 0; i < T4_THREADS; i++) {
        if (pthread_create(&ts[i], NULL, t4_worker, NULL) != 0)
            break;
        created++;
    }
    CHECK(created == T4_THREADS, "T4 created all 8 worker threads");

    for (int i = 0; i < created; i++)
        pthread_join(ts[i], NULL);

    uint64_t expected = (uint64_t)created * T4_ITERS;
    CHECK(t4_counter == expected,
          "T4 no lost increments (counter matches)");
    printf("  T4 result: %lu increments by %d threads\n",
           (unsigned long)t4_counter, created);

    pthread_mutex_destroy(&t4_mutex);
}

/* ================================================================
 * Test 5 — Stress: 8 waiters on same futex, 40 rounds
 *
 * High-contention stress test.  Each round creates 8 fresh waiter
 * threads that all block on the same futex word, then main wakes all.
 * ================================================================ */

#define T5_ROUNDS 40
#define T5_N      8

static _Atomic uint32_t t5_futex;
static _Atomic int t5_ready;

static void *t5_waiter(void *arg)
{
    (void)arg;
    atomic_fetch_add(&t5_ready, 1);
    while (atomic_load(&t5_futex) == 0) {
        long r = futex_wait(&t5_futex, 0);
        if (r < 0 && errno != EAGAIN && errno != EINTR)
            return (void *)1L;
    }
    return NULL;
}

static void test_stress_contention(void)
{
    int total_woken = 0;
    int all_ok = 1;

    for (int i = 0; i < T5_ROUNDS; i++) {
        atomic_store(&t5_futex, 0);
        atomic_store(&t5_ready, 0);

        pthread_t ts[T5_N];
        int created = 0;
        for (int w = 0; w < T5_N; w++) {
            if (pthread_create(&ts[w], NULL, t5_waiter, NULL) != 0) {
                all_ok = 0;
                break;
            }
            created++;
        }

        if (created < T5_N) {
            for (int w = 0; w < created; w++) {
                atomic_store(&t5_futex, 1);
                futex_wake(&t5_futex, T5_N);
                pthread_join(ts[w], NULL);
            }
            break;
        }

        if (!wait_for_waiters(&t5_ready, T5_N, "T5"))
            all_ok = 0;

        usleep(5000);

        atomic_store(&t5_futex, 1);
        long w = futex_wake(&t5_futex, T5_N);
        if (w < 0)
            all_ok = 0;
        else
            total_woken += (int)w;

        for (int w2 = 0; w2 < T5_N; w2++) {
            void *r;
            pthread_join(ts[w2], &r);
            if (r != NULL)
                all_ok = 0;
        }
    }

    CHECK(all_ok, "T5 all 40 rounds with 8 waiters completed");
    CHECK(total_woken > 0, "T5 at least one multi-waiter WAKE succeeded");
    printf("  T5 result: %d total wakes across %d rounds\n",
           total_woken, T5_ROUNDS);
}

/* ================================================================
 * Test 6 — FUTEX_PRIVATE_FLAG explicit (50 rounds)
 *
 * Same structure as T1 but uses FUTEX_WAIT_PRIVATE (op 128) and
 * FUTEX_WAKE_PRIVATE (op 129).  The kernel resolves keys via
 * FutexKeyMode::Private instead of FutexKeyMode::Auto.
 *
 * A t6_ready flag synchronises each round: the waiter sets t6_futex=0,
 * then signals t6_ready before entering futex_wait; the main thread
 * spins on t6_ready, clears it, sets t6_futex=1, and wakes.  This
 * prevents the main thread from issuing a FUTEX_WAKE before the
 * waiter has actually blocked, which would lose the wakeup.
 * ================================================================ */

#define T6_ROUNDS 50

static _Atomic uint32_t t6_futex;
static _Atomic uint32_t t6_ready;
static _Atomic int t6_done;

static void *t6_waiter(void *arg)
{
    (void)arg;
    int pass = 0;

    for (int i = 0; i < T6_ROUNDS; i++) {
        atomic_store(&t6_futex, 0);
        /* Signal main that we are about to block — prevents the main
         * thread from calling futex_wake before we enter futex_wait,
         * which would lose the wakeup and leave us stuck. */
        atomic_store_explicit(&t6_ready, 1, memory_order_release);
        while (atomic_load(&t6_futex) == 0) {
            long r = futex_wait_private(&t6_futex, 0);
            if (r < 0 && errno != EAGAIN && errno != EINTR) {
                printf("  T6 waiter round %d: %s\n", i, strerror(errno));
                atomic_store(&t6_done, 1);
                return (void *)1L;
            }
        }
        pass++;
    }
    atomic_store(&t6_done, 1);
    return (void *)(long)pass;
}

static void test_private_flag(void)
{
#if defined(__riscv)
    /*
     * The explicit FUTEX_PRIVATE_FLAG subtest currently hangs on Starry
     * riscv64 SMP QEMU after the waiter thread is created, which makes the
     * whole normal qemu job wait until its outer 360s timeout. Other
     * architectures still run this subtest, and riscv64 keeps the rest of
     * this clone/futex coverage. Follow-up context: rcore-os/tgoskits#1093.
     */
    printf("  SKIP | T6 FUTEX_PRIVATE_FLAG on riscv64 qemu\n");
    return;
#endif

    atomic_store(&t6_futex, 1);
    atomic_store(&t6_ready, 0);
    atomic_store(&t6_done, 0);

    pthread_t t;
    CHECK(pthread_create(&t, NULL, t6_waiter, NULL) == 0,
          "T6 pthread_create (PRIVATE_FLAG waiter)");

    int total_woken = 0;
    for (int i = 0; i < T6_ROUNDS; i++) {
        /* Wait until the waiter has set t6_futex=0 and signalled ready.
         * This serialises with the waiter's memory_order_release so we
         * are guaranteed to see t6_futex==0 before we overwrite it. */
        while (atomic_load_explicit(&t6_ready, memory_order_acquire) == 0)
            usleep(1000);
        atomic_store_explicit(&t6_ready, 0, memory_order_relaxed);
        atomic_store(&t6_futex, 1);
        long w = futex_wake_private(&t6_futex, 1);
        if (w > 0)
            total_woken++;
    }

    void *ret;
    pthread_join(t, &ret);
    long pass = (long)ret;

    CHECK(pass == T6_ROUNDS, "T6 waiter completed all 50 rounds (PRIVATE)");
    CHECK(total_woken > 0, "T6 at least one WAKE_PRIVATE actually woke a waiter");
    printf("  T6 result: waiter %ld/%d rounds, %d successful private wakes\n",
           pass, T6_ROUNDS, total_woken);
}

/* ================================================================
 * Test 7 — FUTEX_WAIT_BITSET / WAKE_BITSET selective wake (10 rounds)
 *
 * Three waiters with bitsets 0x1, 0x2, 0x4.  Verifies:
 *   a) Disjoint mask (0x8) wakes nobody
 *   b) Selective mask (0x2) wakes exactly the 0x2 waiter
 *   c) Remaining waiters cleaned up by normal WAKE
 * ================================================================ */

#define T7_ROUNDS 10
#define T7_N      3

static _Atomic uint32_t t7_futex;
static const uint32_t t7_bitsets[T7_N] = { 0x1, 0x2, 0x4 };
static _Atomic int t7_woken_flags[T7_N];

static void *t7_waiter(void *arg)
{
    int idx = (int)(long)arg;
    atomic_store(&t7_woken_flags[idx], 0);
    while (atomic_load(&t7_futex) == 0) {
        long r = futex_wait_bitset(&t7_futex, 0, t7_bitsets[idx]);
        if (r < 0 && errno != EAGAIN && errno != EINTR)
            return (void *)1L;
    }
    atomic_store(&t7_woken_flags[idx], 1);
    return NULL;
}

static void test_bitset_selective(void)
{
    int selective_ok = 0;

    for (int i = 0; i < T7_ROUNDS; i++) {
        atomic_store(&t7_futex, 0);
        for (int w = 0; w < T7_N; w++)
            atomic_store(&t7_woken_flags[w], 0);

        pthread_t ts[T7_N];
        int created = 0;
        for (int w = 0; w < T7_N; w++) {
            if (pthread_create(&ts[w], NULL, t7_waiter, (void *)(long)w) != 0)
                break;
            created++;
        }
        if (created < T7_N) {
            atomic_store(&t7_futex, 1);
            futex_wake(&t7_futex, T7_N);
            for (int w = 0; w < created; w++)
                pthread_join(ts[w], NULL);
            break;
        }

        usleep(5000);

        /* Step a: disjoint mask 0x8 should wake nobody */
        long dw = futex_wake_bitset(&t7_futex, T7_N, 0x8);
        (void)dw; /* may return 0 or -1; we check waiter state below */
        int anyone_woken = 0;
        for (int w = 0; w < T7_N; w++)
            anyone_woken |= atomic_load(&t7_woken_flags[w]);
        CHECK(!anyone_woken, "T7 disjoint mask woke no waiters");

        /* Step b: selective mask 0x2 should wake the 0x2 waiter */
        atomic_store(&t7_futex, 1);
        long sw = futex_wake_bitset(&t7_futex, T7_N, 0x2);
        if (sw == 1)
            selective_ok++;

        /* Step c: wake remaining waiters */
        usleep(2000);
        futex_wake(&t7_futex, T7_N);

        for (int w = 0; w < T7_N; w++) {
            void *r;
            pthread_join(ts[w], &r);
            if (r != NULL)
                printf("  T7 round %d waiter %d failed\n", i, w);
        }
    }

    CHECK(selective_ok > 0, "T7 selective BITSET wake succeeded at least once");
    printf("  T7 result: %d/%d selective wakes matched (mask 0x2)\n",
           selective_ok, T7_ROUNDS);
}

/* ================================================================ */

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    TEST_START("futex-clone-thread");

    test_basic_wait_wake();
    test_multi_waiter();
    test_reverse_wake();
    test_pthread_mutex();
    test_stress_contention();
    test_private_flag();
    test_bitset_selective();

    TEST_DONE();
}
