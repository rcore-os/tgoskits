#ifndef PROBE_COMMON_H
#define PROBE_COMMON_H

#include <errno.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>

static inline void fail(const char *operation) {
    perror(operation);
    exit(EXIT_FAILURE);
}

static inline unsigned long parse_number(const char *text, unsigned long maximum) {
    char *end;
    errno = 0;
    unsigned long value = strtoul(text, &end, 10);
    if (errno || text[0] < '0' || text[0] > '9' || *end || value > maximum) {
        fprintf(stderr, "invalid number: %s\n", text);
        exit(EXIT_FAILURE);
    }
    return value;
}

static inline cpu_set_t allowed_cpus(void) {
    cpu_set_t set;
    CPU_ZERO(&set);
    if (sched_getaffinity(0, sizeof(set), &set)) fail("sched_getaffinity");
    return set;
}

static inline void check_cpu(int cpu) {
    int landed = sched_getcpu();
    if (landed < 0) fail("sched_getcpu");
    if (landed != cpu) {
        fprintf(stderr, "CPU placement changed: requested=%d landed=%d\n", cpu, landed);
        exit(EXIT_FAILURE);
    }
}

static inline int pin_cpu(const char *text) {
    int cpu = (int)parse_number(text, CPU_SETSIZE - 1);
    cpu_set_t set = allowed_cpus();
    if (!CPU_ISSET(cpu, &set)) {
        fprintf(stderr, "CPU %d is outside the allowed affinity mask\n", cpu);
        exit(EXIT_FAILURE);
    }
    CPU_ZERO(&set);
    CPU_SET(cpu, &set);
    if (sched_setaffinity(0, sizeof(set), &set)) fail("sched_setaffinity");
    check_cpu(cpu);
    return cpu;
}

static inline uint64_t monotonic_ns(void) {
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts)) fail("clock_gettime");
    return (uint64_t)ts.tv_sec * 1000000000ULL + (uint64_t)ts.tv_nsec;
}

static inline double elapsed_seconds(uint64_t start) {
    uint64_t end = monotonic_ns();
    if (end <= start) {
        fputs("non-positive measurement interval\n", stderr);
        exit(EXIT_FAILURE);
    }
    return (double)(end - start) / 1e9;
}

#endif
