#define _GNU_SOURCE

#include "wakeup-latency-bench.h"

#include <errno.h>
#include <math.h>
#include <pthread.h>
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

static const uint64_t HISTOGRAM_BOUNDS_NS[] = {
    1000,    2000,    5000,    10000,   20000,    50000,   100000,
    200000,  500000,  1000000, 2000000, 5000000, 10000000,
};

uint64_t bench_monotonic_ns(void)
{
    struct timespec now;
    if (syscall(SYS_clock_gettime, CLOCK_MONOTONIC, &now) != 0) {
        perror("clock_gettime");
        exit(EXIT_FAILURE);
    }
    return (uint64_t)now.tv_sec * 1000000000ULL + (uint64_t)now.tv_nsec;
}

int bench_pin_current_thread(int cpu)
{
    cpu_set_t cpuset;
    CPU_ZERO(&cpuset);
    CPU_SET(cpu, &cpuset);
    return pthread_setaffinity_np(pthread_self(), sizeof(cpuset), &cpuset);
}

int bench_apply_policy(enum bench_policy policy, int fifo_priority)
{
    struct sched_param parameter = {
        .sched_priority = policy == BENCH_POLICY_FIFO ? fifo_priority : 0,
    };
    int scheduler = policy == BENCH_POLICY_FIFO ? SCHED_FIFO : SCHED_OTHER;
    return pthread_setschedparam(pthread_self(), scheduler, &parameter);
}

const char *bench_policy_name(enum bench_policy policy)
{
    return policy == BENCH_POLICY_FIFO ? "fifo" : "other";
}

void bench_prefault_samples(uint64_t *samples, size_t sample_count)
{
    memset(samples, 0, sample_count * sizeof(*samples));
    for (size_t index = 0; index < sample_count; index += 512) {
        samples[index] = 0;
    }
}

void bench_print_metadata(const struct bench_config *config)
{
    struct timespec resolution;
    if (syscall(SYS_clock_getres, CLOCK_MONOTONIC, &resolution) != 0) {
        perror("clock_getres");
        exit(EXIT_FAILURE);
    }
    uint64_t resolution_ns =
        (uint64_t)resolution.tv_sec * 1000000000ULL +
        (uint64_t)resolution.tv_nsec;

    uint64_t minimum_pair_ns = UINT64_MAX;
    for (size_t iteration = 0; iteration < 10000; iteration++) {
        uint64_t started = bench_monotonic_ns();
        uint64_t elapsed = bench_monotonic_ns() - started;
        if (elapsed < minimum_pair_ns) {
            minimum_pair_ns = elapsed;
        }
    }

    printf("WAKEUP_LATENCY_METADATA {\"clock\":\"CLOCK_MONOTONIC\","
           "\"clock_read\":\"raw_syscall\","
           "\"clock_resolution_ns\":%llu,\"clock_pair_min_ns\":%llu,"
           "\"sender_cpu\":%d,\"receiver_cpu\":%d,"
           "\"warmup\":%zu,\"handoff_samples\":%zu,"
           "\"timer_samples\":%zu,\"timer_period_ns\":%llu,"
           "\"park_settle_ns\":%llu,\"fifo_priority\":%d}\n",
           (unsigned long long)resolution_ns,
           (unsigned long long)minimum_pair_ns, config->sender_cpu,
           config->receiver_cpu, config->warmup_samples,
           config->handoff_samples, config->timer_samples,
           (unsigned long long)config->timer_period_ns,
           (unsigned long long)config->park_settle_ns,
           config->fifo_priority);
}

static int compare_u64(const void *left, const void *right)
{
    uint64_t a = *(const uint64_t *)left;
    uint64_t b = *(const uint64_t *)right;
    return (a > b) - (a < b);
}

static uint64_t percentile(const uint64_t *samples, size_t sample_count,
                           uint64_t numerator, uint64_t denominator)
{
    size_t index =
        (size_t)(((sample_count - 1) * numerator) / denominator);
    return samples[index];
}

static void print_histogram(const uint64_t *samples, size_t sample_count)
{
    size_t counts[sizeof(HISTOGRAM_BOUNDS_NS) /
                  sizeof(HISTOGRAM_BOUNDS_NS[0]) +
                  1] = { 0 };
    size_t bound_count = sizeof(HISTOGRAM_BOUNDS_NS) /
                         sizeof(HISTOGRAM_BOUNDS_NS[0]);

    for (size_t sample = 0; sample < sample_count; sample++) {
        size_t bucket = 0;
        while (bucket < bound_count &&
               samples[sample] > HISTOGRAM_BOUNDS_NS[bucket]) {
            bucket++;
        }
        counts[bucket]++;
    }

    printf("\"histogram_bounds_ns\":[");
    for (size_t index = 0; index < bound_count; index++) {
        printf("%s%llu", index == 0 ? "" : ",",
               (unsigned long long)HISTOGRAM_BOUNDS_NS[index]);
    }
    printf("],\"histogram_counts\":[");
    for (size_t index = 0; index <= bound_count; index++) {
        printf("%s%zu", index == 0 ? "" : ",", counts[index]);
    }
    printf("]");
}

void bench_print_result(struct latency_result *result)
{
    if (result->sample_count == 0) {
        fprintf(stderr, "%s produced no valid latency samples\n",
                result->case_name);
        exit(EXIT_FAILURE);
    }

    qsort(result->samples, result->sample_count, sizeof(result->samples[0]),
          compare_u64);
    long double sum = 0.0L;
    for (size_t index = 0; index < result->sample_count; index++) {
        sum += (long double)result->samples[index];
    }
    long double mean = sum / (long double)result->sample_count;
    long double squared_error = 0.0L;
    for (size_t index = 0; index < result->sample_count; index++) {
        long double difference = (long double)result->samples[index] - mean;
        squared_error += difference * difference;
    }
    long double standard_deviation =
        sqrtl(squared_error / (long double)result->sample_count);

    printf("WAKEUP_LATENCY_RESULT {\"case\":\"%s\","
           "\"policy\":\"%s\",\"samples\":%zu,"
           "\"attempted\":%zu,\"not_parked\":%zu,"
           "\"missed_deadlines\":%llu,\"min_ns\":%llu,"
           "\"mean_ns\":%.0Lf,\"stddev_ns\":%.0Lf,"
           "\"p50_ns\":%llu,\"p95_ns\":%llu,"
           "\"p99_ns\":%llu,\"p999_ns\":%llu,"
           "\"max_ns\":%llu,",
           result->case_name, bench_policy_name(result->policy),
           result->sample_count, result->attempted_samples,
           result->not_parked_samples,
           (unsigned long long)result->missed_deadlines,
           (unsigned long long)result->samples[0], mean,
           standard_deviation,
           (unsigned long long)percentile(result->samples,
                                          result->sample_count, 50, 100),
           (unsigned long long)percentile(result->samples,
                                          result->sample_count, 95, 100),
           (unsigned long long)percentile(result->samples,
                                          result->sample_count, 99, 100),
           (unsigned long long)percentile(result->samples,
                                          result->sample_count, 999, 1000),
           (unsigned long long)result->samples[result->sample_count - 1]);
    print_histogram(result->samples, result->sample_count);
    printf("}\n");
}

void bench_release_result(struct latency_result *result)
{
    if (result->mapped_storage) {
        if (munmap(result->storage, result->storage_size) != 0) {
            perror("munmap benchmark samples");
            exit(EXIT_FAILURE);
        }
    } else {
        free(result->storage);
    }
    *result = (struct latency_result) { 0 };
}
