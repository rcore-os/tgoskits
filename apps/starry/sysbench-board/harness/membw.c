/*
 * membw — memory bandwidth / first-touch probe, per core.
 *
 * Purpose: explain (or reproduce) StarryOS's ~200x sysbench-memory gap
 * (42 vs 8333 MiB/s). Reports, over a large heap buffer:
 *   - firsttouch_s : time to memset both buffers (page-fault / first-touch cost).
 *                    A pathological per-access fault path shows up as a huge
 *                    first-touch time and/or low warm bandwidth.
 *   - memcpy_GBps  : warm memcpy bandwidth (best of a few reps).
 *   - read_GBps    : warm sequential read (sum) bandwidth.
 *
 * If StarryOS warm bandwidth tracks Linux/frequency, the sysbench-memory number
 * is a sysbench-config artifact; if warm bandwidth itself collapses, it's a real
 * StarryOS memory-path problem (uncached mapping, fault-per-touch, etc.).
 *
 * Build: aarch64 glibc. Usage: membw [core_index] [size_MiB]
 */
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
#include <sched.h>

static inline uint64_t rd_cntvct(void) {
    uint64_t v;
    __asm__ volatile("isb; mrs %0, cntvct_el0" : "=r"(v) :: "memory");
    return v;
}
static inline uint64_t rd_cntfrq(void) {
    uint64_t v;
    __asm__ volatile("mrs %0, cntfrq_el0" : "=r"(v));
    return v;
}

int main(int argc, char **argv) {
    int core = (argc > 1) ? atoi(argv[1]) : -1;
    size_t mb = (argc > 2) ? strtoul(argv[2], NULL, 10) : 256;
    if (core >= 0) {
        cpu_set_t set;
        CPU_ZERO(&set);
        CPU_SET(core, &set);
        (void)sched_setaffinity(0, sizeof set, &set);
        for (volatile int i = 0; i < 2000000; i++) { }
    }
    size_t n = mb * 1024ULL * 1024ULL;
    uint64_t frq = rd_cntfrq();
    if (!frq) frq = 24000000;

    char *a = malloc(n), *b = malloc(n);
    if (!a || !b) {
        printf("MEMBW core=%d landed=%d mb=%zu alloc_fail\n", core, sched_getcpu(), mb);
        return 1;
    }

    uint64_t t0 = rd_cntvct();
    memset(a, 1, n);
    memset(b, 2, n);
    uint64_t t1 = rd_cntvct();
    double ft = (double)(t1 - t0) / (double)frq;

    double best = 1e30;
    for (int r = 0; r < 3; r++) {
        uint64_t c0 = rd_cntvct();
        memcpy(a, b, n);
        uint64_t c1 = rd_cntvct();
        double s = (double)(c1 - c0) / (double)frq;
        if (s < best) best = s;
    }
    double cp_gbps = (double)n / best / 1e9;

    volatile uint64_t acc = 0;
    uint64_t *p = (uint64_t *)a;
    size_t words = n / 8;
    double rbest = 1e30;
    for (int r = 0; r < 3; r++) {
        uint64_t c0 = rd_cntvct();
        uint64_t s2 = 0;
        for (size_t i = 0; i < words; i++) s2 += p[i];
        acc ^= s2;
        uint64_t c1 = rd_cntvct();
        double s = (double)(c1 - c0) / (double)frq;
        if (s < rbest) rbest = s;
    }
    double rd_gbps = (double)n / rbest / 1e9;

    printf("MEMBW core=%d landed=%d mb=%zu firsttouch_s=%.4f memcpy_GBps=%.3f "
           "read_GBps=%.3f acc=%llx\n",
           core, sched_getcpu(), mb, ft, cp_gbps, rd_gbps, (unsigned long long)acc);
    return 0;
}
