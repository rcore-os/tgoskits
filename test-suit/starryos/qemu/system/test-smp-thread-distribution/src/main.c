/*
 * test-smp-thread-distribution.c -- SMP thread-distribution regression test
 *
 * Proves that newly created threads actually get spread across distinct
 * CPUs instead of piling up on a single core (the boot CPU).
 *
 * Before the axtask SMP-distribution fix, every worker thread stayed on
 * the same core it was created on (the boot core), so the set of CPUs
 * observed across all workers over the run would contain exactly one
 * entry. After the fix, new/cloned tasks are round-robined across the
 * online CPUs, so workers should be observed running on >= 2 distinct
 * CPUs.
 *
 * Method:
 *   1. Spawn n = max(2, online CPU count) worker threads, gated behind a
 *      shared atomic "go" start barrier so they all begin spinning at
 *      roughly the same time.
 *   2. Each worker spins for a fixed wall-clock duration, repeatedly
 *      sampling its current CPU (via SYS_getcpu, the same syscall used by
 *      the syscall-test-getcpu case) and OR-ing the CPU id into a shared
 *      atomic bitmask.
 *   3. After joining all workers, popcount the bitmask: it must contain
 *      >= 2 distinct CPUs. We deliberately only require >= 2 (not
 *      == ncpu) so ordinary scheduling jitter/imbalance can't make this
 *      flaky -- only the all-on-one-core pile-up bug can fail it.
 */
#define _GNU_SOURCE
#include "test_framework.h"
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdint.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

enum {
    MAX_WORKERS = 64,
    SPIN_DURATION_MS = 1500,
};

static atomic_int go = 0;
static _Atomic uint64_t seen_mask = 0;

/* Read the CPU the calling thread is currently running on via the raw
 * getcpu(2) syscall, mirroring syscall-test-getcpu's approach so this
 * does not depend on sched_getcpu(3) being declared by the libc headers. */
static long current_cpu(void)
{
    unsigned cpu = 0;
    long ret = syscall(SYS_getcpu, &cpu, NULL, NULL);
    if (ret != 0)
        return -1;
    return (long)cpu;
}

static long monotonic_ms(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static int popcount64(uint64_t v)
{
    int count = 0;
    while (v) {
        v &= v - 1;
        count++;
    }
    return count;
}

static void *worker(void *arg)
{
    (void)arg;

    /* Start barrier: wait until main has finished creating every worker
     * so they all spin concurrently rather than serializing through
     * creation order. */
    while (!atomic_load_explicit(&go, memory_order_acquire)) {
        sched_yield();
    }

    long deadline = monotonic_ms() + SPIN_DURATION_MS;

    /* CPU-bound sampling loop: each iteration issues a real syscall
     * (getcpu), so the loop cannot be optimized away and keeps the
     * thread runnable for the whole window. */
    while (monotonic_ms() < deadline) {
        long cpu = current_cpu();
        if (cpu >= 0 && cpu < 64) {
            atomic_fetch_or_explicit(&seen_mask, (uint64_t)1u << cpu,
                                      memory_order_relaxed);
        }
    }

    return NULL;
}

int main(void)
{
    TEST_START("SMP thread distribution across CPUs");

    long ncpu = sysconf(_SC_NPROCESSORS_ONLN);
    if (ncpu < 1)
        ncpu = 1;

    int n = (int)(ncpu > 1 ? ncpu : 2);
    if (n > MAX_WORKERS)
        n = MAX_WORKERS;

    pthread_t threads[MAX_WORKERS];
    int created = 0;

    for (int i = 0; i < n; i++) {
        int err = pthread_create(&threads[i], NULL, worker, NULL);
        CHECK(err == 0, "pthread_create for worker succeeds");
        if (err != 0)
            break;
        created++;
    }

    /* Release every created worker together. */
    atomic_store_explicit(&go, 1, memory_order_release);

    for (int i = 0; i < created; i++) {
        pthread_join(threads[i], NULL);
    }

    uint64_t mask = atomic_load_explicit(&seen_mask, memory_order_relaxed);
    int distinct = popcount64(mask);

    printf("  INFO | online_cpus=%ld workers=%d seen_mask=0x%llx distinct=%d\n",
           ncpu, created, (unsigned long long)mask, distinct);

    CHECK(distinct >= 2, "threads ran on >= 2 CPUs (SMP distribution)");

    TEST_DONE();
}
