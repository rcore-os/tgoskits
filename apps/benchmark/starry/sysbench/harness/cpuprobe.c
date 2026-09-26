/* Pinned integer throughput measured with the operating system monotonic clock. */
#define _GNU_SOURCE
#include <string.h>
#include "probe_common.h"

static double timed_work(uint64_t iters, uint64_t *sink) {
    uint64_t start = monotonic_ns();
    uint64_t x = start | 1;
    for (uint64_t i = 0; i < iters; i++) {
        x = x * 6364136223846793005ULL + 1442695040888963407ULL;
        x ^= x >> 29;
        x *= 0xff51afd7ed558ccdULL;
        x ^= x >> 32;
    }
    /* Keep all work inside the timed interval, including under LTO. */
    __asm__ volatile("" : "+r"(x) :: "memory");
    double sec = elapsed_seconds(start);
    *sink ^= x;
    return sec;
}

int main(int argc, char **argv) {
    if (argc != 2) {
        fputs("usage: cpuprobe CPU | --list\n", stderr);
        return EXIT_FAILURE;
    }
    if (!strcmp(argv[1], "--list")) {
        cpu_set_t set = allowed_cpus();
        int count = 0;
        for (int cpu = 0; cpu < CPU_SETSIZE; cpu++) {
            if (CPU_ISSET(cpu, &set)) printf("%s%d", count++ ? " " : "", cpu);
        }
        putchar('\n');
        return count ? EXIT_SUCCESS : EXIT_FAILURE;
    }
    int cpu = pin_cpu(argv[1]);
    uint64_t iters = 1000000, sink = 0;
    while (timed_work(iters, &sink) < 0.1 && iters < (1ULL << 32)) iters *= 2;
    double sec = timed_work(iters, &sink);
    check_cpu(cpu);
    printf("CPUPROBE req=%d landed=%d iters=%llu sec=%.9f ips=%.0f sink=%llx\n",
           cpu, cpu, (unsigned long long)iters, sec, iters / sec,
           (unsigned long long)sink);
    return EXIT_SUCCESS;
}
