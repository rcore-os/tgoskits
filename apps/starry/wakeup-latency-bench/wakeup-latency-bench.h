#ifndef WAKEUP_LATENCY_BENCH_H
#define WAKEUP_LATENCY_BENCH_H

#include <stddef.h>
#include <stdint.h>

enum bench_policy {
    BENCH_POLICY_OTHER,
    BENCH_POLICY_FIFO,
};

struct bench_config {
    size_t warmup_samples;
    size_t handoff_samples;
    size_t timer_samples;
    uint64_t park_settle_ns;
    uint64_t timer_period_ns;
    int sender_cpu;
    int receiver_cpu;
    int fifo_priority;
};

struct latency_result {
    const char *case_name;
    enum bench_policy policy;
    uint64_t *samples;
    size_t sample_count;
    size_t attempted_samples;
    size_t not_parked_samples;
    uint64_t missed_deadlines;
    void *storage;
    size_t storage_size;
    int mapped_storage;
};

uint64_t bench_monotonic_ns(void);
int bench_pin_current_thread(int cpu);
int bench_apply_policy(enum bench_policy policy, int fifo_priority);
const char *bench_policy_name(enum bench_policy policy);
void bench_prefault_samples(uint64_t *samples, size_t sample_count);
void bench_print_metadata(const struct bench_config *config);
void bench_print_result(struct latency_result *result);
void bench_release_result(struct latency_result *result);

int bench_thread_handoff(const struct bench_config *config,
                         enum bench_policy policy, int same_cpu,
                         struct latency_result *result);
int bench_process_handoff(const struct bench_config *config,
                          enum bench_policy policy,
                          struct latency_result *result);
int bench_absolute_timer(const struct bench_config *config,
                         enum bench_policy policy,
                         struct latency_result *result);

#endif
