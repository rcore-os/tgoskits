/* kcov: SMP concurrent coverage test — proves per-CPU guard isolation.
 *
 * Repeats the two-phase measurement across multiple rounds to prove the
 * per-CPU guard consistently prevents cross-CPU coverage loss.
 *
 *   Phase 1 — Sequential baselines:
 *     Pin to CPU 0, run do_work() (tight getpid() loop), record coverage.
 *     Pin to CPU 1, run do_work(), record coverage.
 *     Fail if buf[0] == BUF_ENTRIES (buffer overflow → baseline invalid).
 *
 *   Phase 2 — Concurrent (pthread_barrier release):
 *     Thread A pinned to CPU 0, Thread B pinned to CPU 1.
 *     Both released simultaneously via pthread_barrier_wait().
 *     Each records its coverage count.
 *
 *   Phase 3 — Per-round assertions:
 *     Each concurrent count >= 70% of its sequential baseline.
 *     Combined concurrent >= 70% of summed baselines.
 *
 * The workload uses getpid() (real syscall on musl/Alpine).  KCOV traces
 * kernel basic blocks, so each getpid() generates several trace_pc calls.
 *
 * With a per-CPU guard both CPUs trace independently — cross-CPU contention
 * is zero, so each thread keeps ~90%+ of baseline.  A single global guard
 * would cause one CPU to lose 50%+ when both trace simultaneously.
 */
#include "test_framework.h"
#include <fcntl.h>
#include <pthread.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

#define KCOV_INIT_TRACE _IOR('c', 1, unsigned long)
#define KCOV_ENABLE     _IO('c', 100)
#define KCOV_DISABLE    _IO('c', 101)
#define KCOV_TRACE_PC   0

#define ROUNDS      20
#define WORK_LOOPS  500
#define BUF_ENTRIES 65536

/* Busy-workload: tight getpid() loop that generates kernel coverage entries.
 * getpid() on musl (Alpine) makes a real syscall every time — no caching. */
static void do_work(uint64_t iterations) {
    for (uint64_t i = 0; i < iterations; i++) {
        getpid();
    }
}

/* Run the workload on a specific CPU and return the coverage entry count.
 * Returns UINT64_MAX on any error. */
static uint64_t run_sequential(int cpu, uint64_t iterations) {
    cpu_set_t cpuset;
    CPU_ZERO(&cpuset);
    CPU_SET(cpu, &cpuset);
    if (sched_setaffinity(0, sizeof(cpuset), &cpuset) != 0) {
        return UINT64_MAX;
    }

    int fd = open("/dev/kcov", O_RDWR);
    if (fd < 0) return UINT64_MAX;

    if (ioctl(fd, KCOV_INIT_TRACE, BUF_ENTRIES)) {
        close(fd);
        return UINT64_MAX;
    }

    size_t sz = BUF_ENTRIES * sizeof(uint64_t);
    uint64_t *buf = mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (buf == MAP_FAILED) {
        close(fd);
        return UINT64_MAX;
    }

    if (ioctl(fd, KCOV_ENABLE, KCOV_TRACE_PC)) {
        munmap(buf, sz);
        close(fd);
        return UINT64_MAX;
    }

    do_work(iterations);

    uint64_t count = buf[0];

    ioctl(fd, KCOV_DISABLE, 0);
    munmap(buf, sz);
    close(fd);
    return count;
}

/* Per-thread data for the concurrent phase */
typedef struct {
    int               cpu;
    uint64_t          count;   /* coverage entries recorded */
    int               ok;      /* 1 = success */
    pthread_barrier_t *barrier;
} thr_t;

static void *thread_concurrent(void *arg) {
    thr_t *s = (thr_t *)arg;
    s->ok = 0;

    cpu_set_t cpuset;
    CPU_ZERO(&cpuset);
    CPU_SET(s->cpu, &cpuset);
    if (sched_setaffinity(0, sizeof(cpuset), &cpuset) != 0) {
        return NULL;
    }

    int fd = open("/dev/kcov", O_RDWR);
    if (fd < 0) return NULL;

    if (ioctl(fd, KCOV_INIT_TRACE, BUF_ENTRIES)) {
        close(fd);
        return NULL;
    }

    size_t sz = BUF_ENTRIES * sizeof(uint64_t);
    uint64_t *buf = mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (buf == MAP_FAILED) {
        close(fd);
        return NULL;
    }

    if (ioctl(fd, KCOV_ENABLE, KCOV_TRACE_PC)) {
        munmap(buf, sz);
        close(fd);
        return NULL;
    }

    /* Both threads released simultaneously */
    pthread_barrier_wait(s->barrier);

    do_work(WORK_LOOPS);

    s->count = buf[0];

    ioctl(fd, KCOV_DISABLE, 0);
    munmap(buf, sz);
    close(fd);
    s->ok = 1;
    return NULL;
}

int main(void) {
    TEST_START("KCOV SMP: per-CPU guard isolation (20 rounds)");

    for (int r = 0; r < ROUNDS; r++) {
        printf("\n  === Round %d/%d ===\n", r + 1, ROUNDS);

        /* ---- Phase 1: sequential baselines ---- */
        uint64_t seq_a = run_sequential(0, WORK_LOOPS);
        uint64_t seq_b = run_sequential(1, WORK_LOOPS);

        CHECK(seq_a != UINT64_MAX && seq_a > 100,
              "CPU 0 sequential baseline > 100 (reject cached-getpid regression)");
        CHECK(seq_b != UINT64_MAX && seq_b > 100,
              "CPU 1 sequential baseline > 100 (reject cached-getpid regression)");

        /* Overflow guard: buffer must not be full, otherwise the baseline
         * measurement is truncated and the 70% threshold is meaningless. */
        CHECK(seq_a < BUF_ENTRIES,
              "CPU 0 baseline did not overflow buffer");
        CHECK(seq_b < BUF_ENTRIES,
              "CPU 1 baseline did not overflow buffer");

        printf("    seq[CPU0] = %lu,  seq[CPU1] = %lu\n", seq_a, seq_b);

        /* ---- Phase 2: concurrent ---- */
        pthread_barrier_t barrier;
        pthread_barrier_init(&barrier, NULL, 2);

        pthread_t ta, tb;
        thr_t sa = { .cpu = 0, .barrier = &barrier };
        thr_t sb = { .cpu = 1, .barrier = &barrier };

        CHECK(pthread_create(&ta, NULL, thread_concurrent, &sa) == 0,
              "pthread_create CPU 0");
        CHECK(pthread_create(&tb, NULL, thread_concurrent, &sb) == 0,
              "pthread_create CPU 1");

        pthread_join(ta, NULL);
        pthread_join(tb, NULL);

        CHECK(sa.ok, "thread on CPU 0 completed");
        CHECK(sb.ok, "thread on CPU 1 completed");

        uint64_t con_a = sa.count;
        uint64_t con_b = sb.count;
        printf("    con[CPU0] = %lu,  con[CPU1] = %lu\n", con_a, con_b);

        /* ---- Phase 3: assertions ---- */

        /* Each concurrent run must achieve >= 70% of its sequential baseline.
         *
         * Small losses (10-20%) are expected from:
         *   - cache-line bouncing on the shared kcov buffer count word
         *   - scheduler jitter
         *   - self-recursion from the instrumented body of kcov_trace_pc_impl
         *
         * A global guard produces >> 50% loss on at least one CPU. */
        uint64_t thr_a = seq_a * 70 / 100;
        uint64_t thr_b = seq_b * 70 / 100;

        CHECK(con_a >= thr_a,
              "CPU 0 concurrent coverage >= 70% of sequential baseline");
        CHECK(con_b >= thr_b,
              "CPU 1 concurrent coverage >= 70% of sequential baseline");

        /* Combined total >= 70% of summed baselines */
        CHECK(con_a + con_b >= (seq_a + seq_b) * 70 / 100,
              "combined concurrent coverage >= 70% of combined baseline");

        pthread_barrier_destroy(&barrier);
    }

    TEST_DONE();
}
