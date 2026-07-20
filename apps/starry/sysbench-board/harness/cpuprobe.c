/*
 * cpuprobe - per-core CPU identity / frequency / placement probe.
 *
 * Answers the three things we could previously only INFER about a StarryOS core:
 *   1. Core type          - MIDR_EL1 (part 0xd05 = Cortex-A55, 0xd0b = A76).
 *   2. Real frequency      - PMCCNTR_EL0 (cycle counter) over a CNTVCT_EL0 window
 *                            => exact MHz, IF the kernel exposes EL0 PMU access.
 *   3. Placement worked?   - sched_getcpu() vs the requested core.
 *
 * MIDR_EL1 / PMCCNTR_EL0 EL0 reads are UNDEFINED on kernels that don't emulate/
 * enable them. The risky reads run in a FORKED CHILD which also installs SIGILL/
 * SIGSEGV/SIGBUS/SIGTRAP guards, so it recovers whatever partial data it can and
 * always exits cleanly; the parent still reports the (always-safe) CNTVCT-timed
 * throughput regardless. Compile with -DNO_SYSREG to omit the sysreg probe (used
 * for the host smoke-test, since a nested hypervisor may kill the VM on these
 * instructions rather than raising a catchable signal - a test-env quirk only).
 *
 * Build: aarch64 glibc. Usage: cpuprobe [core_index]
 */
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
#include <sched.h>
#include <unistd.h>
#include <signal.h>
#include <setjmp.h>
#include <sys/wait.h>

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

/* Fixed CPU-bound integer kernel. Seeded so the result depends on runtime state
 * (the start-timer value at the call site), which — together with a "+r" barrier
 * on the result before the end-timer read — pins the loop strictly inside the
 * timed window (a pure function would otherwise be hoisted out by -O2). */
static uint64_t work(uint64_t iters, uint64_t seed) {
    uint64_t x = seed | 1;
    for (uint64_t i = 0; i < iters; i++) {
        x = x * 6364136223846793005ULL + 1442695040888963407ULL;
        x ^= x >> 29;
        x *= 0xff51afd7ed558ccdULL;
        x ^= x >> 32;
    }
    return x;
}

/* Time `work(iters)` with the loop provably inside the CNTVCT window. */
static double timed_work(uint64_t iters, uint64_t frq, uint64_t *sink) {
    uint64_t a = rd_cntvct();
    uint64_t r = work(iters, a);
    __asm__ volatile("" : "+r"(r) :: "memory");
    uint64_t b = rd_cntvct();
    *sink ^= r;
    return (double)(b - a) / (double)frq;
}

#ifndef NO_SYSREG
static sigjmp_buf g_cjb;
static void child_trap(int sig) { (void)sig; siglongjmp(g_cjb, 1); }

/* Child-isolated, signal-guarded read of MIDR (core type) and PMCCNTR (MHz).
 * Fills the out-params with whatever succeeds. Returns 1 if the child produced
 * any output, 0 if it died before writing. */
static int probe_sysregs(uint64_t iters, int *midr_ok, unsigned *part,
                         int *pmc_ok, double *mhz) {
    int fd[2];
    if (pipe(fd) != 0) return 0;
    pid_t pid = fork();
    if (pid < 0) { close(fd[0]); close(fd[1]); return 0; }
    if (pid == 0) {
        close(fd[0]);
        struct sigaction sa;
        memset(&sa, 0, sizeof sa);
        sa.sa_handler = child_trap;
        sigaction(SIGILL, &sa, NULL);
        sigaction(SIGSEGV, &sa, NULL);
        sigaction(SIGBUS, &sa, NULL);
        sigaction(SIGTRAP, &sa, NULL);

        int mok = 0, pok = 0;
        unsigned long long midr = 0;
        double child_mhz = 0.0;
        if (sigsetjmp(g_cjb, 1) == 0) {
            __asm__ volatile("mrs %0, midr_el1" : "=r"(midr));
            mok = 1;
        }
        if (sigsetjmp(g_cjb, 1) == 0) {
            uint64_t c0, c1, t0, t1, frq = rd_cntfrq();
            if (!frq) frq = 24000000;
            __asm__ volatile("mrs %0, pmccntr_el0" : "=r"(c0));
            t0 = rd_cntvct();
            uint64_t s = work(iters, t0);
            __asm__ volatile("" : "+r"(s) :: "memory");
            t1 = rd_cntvct();
            __asm__ volatile("mrs %0, pmccntr_el0" : "=r"(c1));
            (void)s;
            double cs = (double)(t1 - t0) / (double)frq;
            if (c1 > c0 && cs > 0) { child_mhz = (double)(c1 - c0) / cs / 1e6; pok = 1; }
        }
        unsigned p = mok ? (unsigned)((midr >> 4) & 0xfff) : 0;
        char buf[128];
        int len = snprintf(buf, sizeof buf, "%d %u %d %.1f\n", mok, p, pok, child_mhz);
        ssize_t w = write(fd[1], buf, len);
        (void)w;
        _exit(0);
    }
    close(fd[1]);
    char buf[160];
    ssize_t n = read(fd[0], buf, sizeof buf - 1);
    close(fd[0]);
    int st = 0;
    waitpid(pid, &st, 0);
    if (n <= 0) return 0;
    buf[n] = 0;
    int mok = 0, pok = 0;
    unsigned p = 0;
    double m = 0.0;
    if (sscanf(buf, "%d %u %d %lf", &mok, &p, &pok, &m) != 4) return 0;
    *midr_ok = mok; *part = p; *pmc_ok = pok; *mhz = m;
    return 1;
}
#endif

int main(int argc, char **argv) {
    int req = (argc > 1) ? atoi(argv[1]) : -1;
    if (req >= 0) {
        cpu_set_t set;
        CPU_ZERO(&set);
        CPU_SET(req, &set);
        (void)sched_setaffinity(0, sizeof set, &set); /* landed field reveals if honored */
    }
    for (volatile int i = 0; i < 2000000; i++) { }
    int landed = sched_getcpu();

    uint64_t frq = rd_cntfrq();
    if (!frq) frq = 24000000;

    uint64_t iters = 2000000, sink = 0;
    double sec;
    for (;;) {
        sec = timed_work(iters, frq, &sink);
        if (sec > 0.35 || iters > (1ULL << 34)) break;
        iters *= 2;
    }
    sec = timed_work(iters, frq, &sink); /* measured run */
    double ips = sec > 0 ? iters / sec : 0;

    int midr_ok = 0, pmc_ok = 0;
    unsigned part = 0;
    double mhz_pmc = 0.0;
#ifndef NO_SYSREG
    (void)probe_sysregs(iters, &midr_ok, &part, &pmc_ok, &mhz_pmc);
#endif

    printf("CPUPROBE req=%d landed=%d cntfrq=%llu iters=%llu sec=%.4f ips=%.0f "
           "midr_ok=%d part=0x%x pmc_ok=%d mhz_pmc=%.1f sink=%llx\n",
           req, landed, (unsigned long long)frq, (unsigned long long)iters, sec, ips,
           midr_ok, part, pmc_ok, mhz_pmc, (unsigned long long)sink);
    return 0;
}
