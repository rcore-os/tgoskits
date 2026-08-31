// SPDX-License-Identifier: Apache-2.0
// A deterministic Linux-guest periodic wake measurement for AxVisor profiles.

#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>

enum {
    round_count = 3,
    sample_count = 2000,
    period_ns = 1000000,
};

struct cpu_stat {
    uint64_t total;
    uint64_t idle;
};

static uint64_t monotonic_ns(void) {
    struct timespec timestamp;
    if (clock_gettime(CLOCK_MONOTONIC, &timestamp) != 0) {
        return 0;
    }
    return (uint64_t)timestamp.tv_sec * UINT64_C(1000000000) + (uint64_t)timestamp.tv_nsec;
}

static int compare_u64(const void *left, const void *right) {
    const uint64_t left_value = *(const uint64_t *)left;
    const uint64_t right_value = *(const uint64_t *)right;
    return (left_value > right_value) - (left_value < right_value);
}

static uint64_t percentile(const uint64_t *samples, size_t numerator) {
    size_t index = (sample_count * numerator + 99) / 100;
    if (index == 0) {
        index = 1;
    }
    return samples[index - 1];
}

static int read_proc_stat(struct cpu_stat cpus[2], uint64_t *context_switches) {
    FILE *file = fopen("/proc/stat", "r");
    char line[512];
    unsigned int cpu = 0;

    if (file == NULL) {
        return 1;
    }
    while (fgets(line, sizeof(line), file) != NULL) {
        uint64_t values[10] = {0};
        if (sscanf(
                line,
                "cpu%u %" SCNu64 " %" SCNu64 " %" SCNu64 " %" SCNu64 " %" SCNu64
                " %" SCNu64 " %" SCNu64 " %" SCNu64 " %" SCNu64 " %" SCNu64,
                &cpu,
                &values[0],
                &values[1],
                &values[2],
                &values[3],
                &values[4],
                &values[5],
                &values[6],
                &values[7],
                &values[8],
                &values[9]) == 11) {
            if (cpu < 2) {
                cpus[cpu].total = 0;
                for (size_t index = 0; index < 10; index++) {
                    cpus[cpu].total += values[index];
                }
                cpus[cpu].idle = values[3] + values[4];
            }
            continue;
        }
        if (sscanf(line, "ctxt %" SCNu64, context_switches) == 1) {
            continue;
        }
    }
    return fclose(file) == 0 ? 0 : 1;
}

static uint64_t absolute_difference(uint64_t left, uint64_t right) {
    return left >= right ? left - right : right - left;
}

static int measure_round(unsigned int round) {
    uint64_t samples[sample_count];
    uint64_t jitter_samples[sample_count];
    uint64_t deadline = monotonic_ns();
    uint64_t previous_wake = deadline;
    uint64_t misses = 0;
    struct cpu_stat start_cpus[2] = {0};
    struct cpu_stat end_cpus[2] = {0};
    uint64_t start_context_switches = 0;
    uint64_t end_context_switches = 0;

    if (deadline == 0 || read_proc_stat(start_cpus, &start_context_switches) != 0) {
        return 1;
    }
    for (size_t index = 0; index < sample_count; index++) {
        deadline += period_ns;
        struct timespec wake_at = {
            .tv_sec = (time_t)(deadline / UINT64_C(1000000000)),
            .tv_nsec = (long)(deadline % UINT64_C(1000000000)),
        };
        if (clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &wake_at, NULL) != 0) {
            return 1;
        }
        uint64_t now = monotonic_ns();
        if (now == 0) {
            return 1;
        }
        samples[index] = now > deadline ? now - deadline : 0;
        jitter_samples[index] =
            absolute_difference(absolute_difference(now, previous_wake), period_ns);
        previous_wake = now;
        if (samples[index] > period_ns) {
            misses++;
        }
    }

    if (read_proc_stat(end_cpus, &end_context_switches) != 0) {
        return 1;
    }
    qsort(samples, sample_count, sizeof(samples[0]), compare_u64);
    qsort(jitter_samples, sample_count, sizeof(jitter_samples[0]), compare_u64);
    printf(
        "AXVISOR_RT_LATENCY_BENCH round=%u/%u samples=%u period_ns=%u p50_ns=%" PRIu64
        " p95_ns=%" PRIu64 " p99_ns=%" PRIu64 " max_ns=%" PRIu64
        " jitter_p50_ns=%" PRIu64 " jitter_p95_ns=%" PRIu64
        " jitter_p99_ns=%" PRIu64 " jitter_max_ns=%" PRIu64
        " deadline_miss=%" PRIu64 " guest_cpu0_total_ticks=%" PRIu64
        " guest_cpu0_idle_ticks=%" PRIu64 " guest_cpu1_total_ticks=%" PRIu64
        " guest_cpu1_idle_ticks=%" PRIu64 " guest_context_switches=%" PRIu64 "\n",
        round,
        round_count,
        sample_count,
        period_ns,
        percentile(samples, 50),
        percentile(samples, 95),
        percentile(samples, 99),
        samples[sample_count - 1],
        percentile(jitter_samples, 50),
        percentile(jitter_samples, 95),
        percentile(jitter_samples, 99),
        jitter_samples[sample_count - 1],
        misses,
        end_cpus[0].total - start_cpus[0].total,
        end_cpus[0].idle - start_cpus[0].idle,
        end_cpus[1].total - start_cpus[1].total,
        end_cpus[1].idle - start_cpus[1].idle,
        end_context_switches - start_context_switches);
    return 0;
}

int main(void) {
    for (unsigned int round = 1; round <= round_count; round++) {
        if (measure_round(round) != 0) {
            return 1;
        }
    }
    return 0;
}
