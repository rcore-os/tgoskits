/* First-touch latency and median warm copy/read bandwidth, pinned to one CPU. */
#define _GNU_SOURCE
#include <string.h>
#include "probe_common.h"

static double median(double samples[3]) {
    for (int i = 0; i < 2; i++) {
        for (int j = i + 1; j < 3; j++) {
            if (samples[j] < samples[i]) {
                double tmp = samples[i];
                samples[i] = samples[j];
                samples[j] = tmp;
            }
        }
    }
    return samples[1];
}

int main(int argc, char **argv) {
    if (argc != 3) {
        fputs("usage: membw CPU SIZE_MIB\n", stderr);
        return EXIT_FAILURE;
    }
    int cpu = pin_cpu(argv[1]);
    size_t mb = parse_number(argv[2], SIZE_MAX / (2 * 1024 * 1024));
    if (!mb) {
        fputs("SIZE_MIB must be positive\n", stderr);
        return EXIT_FAILURE;
    }
    size_t n = mb * 1024 * 1024;
    unsigned char *a = malloc(n), *b = malloc(n);
    if (!a || !b) {
        free(a);
        free(b);
        fail("malloc");
    }
    uint64_t start = monotonic_ns();
    memset(a, 1, n);
    memset(b, 2, n);
    /* Both initial writes must complete before the first-touch timer ends. */
    __asm__ volatile("" :: "r"(a), "r"(b) : "memory");
    double firsttouch = elapsed_seconds(start);
    double copies[3], reads[3];
    uint64_t acc = 0;
    for (int sample = 0; sample < 3; sample++) {
        start = monotonic_ns();
        memcpy(a, b, n);
        /* Each repetition must remain observable even though it overwrites a. */
        __asm__ volatile("" :: "r"(a) : "memory");
        copies[sample] = elapsed_seconds(start);
        start = monotonic_ns();
        uint64_t sum = 0;
        const uint64_t *words = (const uint64_t *)a;
        for (size_t i = 0; i < n / sizeof(*words); i++) sum += words[i];
        __asm__ volatile("" : "+r"(sum) :: "memory");
        reads[sample] = elapsed_seconds(start);
        acc ^= sum;
    }
    check_cpu(cpu);
    printf("MEMBW core=%d landed=%d mb=%zu firsttouch_s=%.9f memcpy_GBps=%.6f "
           "read_GBps=%.6f acc=%llx\n", cpu, cpu, mb, firsttouch,
           n / median(copies) / 1e9, n / median(reads) / 1e9,
           (unsigned long long)acc);
    free(a);
    free(b);
    return EXIT_SUCCESS;
}
