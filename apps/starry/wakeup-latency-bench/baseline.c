#define _GNU_SOURCE

#include "wakeup-latency-bench.h"

#include <errno.h>
#include <linux/futex.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <unistd.h>

enum baseline_operation {
    BASELINE_CLOCK_PAIR,
    BASELINE_FUTEX_WAIT_MISMATCH,
    BASELINE_FUTEX_WAKE_EMPTY,
};

static int run_operation(enum baseline_operation operation,
                         _Atomic uint32_t *futex_word)
{
    switch (operation) {
    case BASELINE_CLOCK_PAIR:
        return 0;
    case BASELINE_FUTEX_WAIT_MISMATCH: {
        long result = syscall(SYS_futex, futex_word,
                              FUTEX_WAIT | FUTEX_PRIVATE_FLAG, 0, NULL, NULL,
                              0);
        return result == -1 && errno == EAGAIN ? 0 : -1;
    }
    case BASELINE_FUTEX_WAKE_EMPTY:
        return syscall(SYS_futex, futex_word,
                       FUTEX_WAKE | FUTEX_PRIVATE_FLAG, 1, NULL, NULL, 0) == 0
                   ? 0
                   : -1;
    }
    errno = EINVAL;
    return -1;
}

static int bench_syscall_baseline(const struct bench_config *config,
                                  enum bench_policy policy,
                                  enum baseline_operation operation,
                                  const char *case_name,
                                  struct latency_result *result)
{
    if (config->handoff_samples > SIZE_MAX / sizeof(uint64_t)) {
        errno = EOVERFLOW;
        return -1;
    }
    size_t storage_size = config->handoff_samples * sizeof(uint64_t);
    uint64_t *samples = malloc(storage_size);
    if (samples == NULL) {
        return -1;
    }
    bench_prefault_samples(samples, config->handoff_samples);

    _Atomic uint32_t futex_word = 1;
    size_t total_samples = config->warmup_samples + config->handoff_samples;
    for (size_t iteration = 0; iteration < total_samples; iteration++) {
        uint64_t started = bench_monotonic_ns();
        int operation_result = run_operation(operation, &futex_word);
        int operation_errno = errno;
        uint64_t elapsed = bench_monotonic_ns() - started;
        if (operation_result != 0) {
            errno = operation_errno == 0 ? EPROTO : operation_errno;
            free(samples);
            return -1;
        }
        if (iteration >= config->warmup_samples) {
            samples[iteration - config->warmup_samples] = elapsed;
        }
    }

    *result = (struct latency_result) {
        .case_name = case_name,
        .policy = policy,
        .samples = samples,
        .sample_count = config->handoff_samples,
        .attempted_samples = config->handoff_samples,
        .storage = samples,
        .storage_size = storage_size,
    };
    return 0;
}

int bench_clock_pair(const struct bench_config *config,
                     enum bench_policy policy, struct latency_result *result)
{
    return bench_syscall_baseline(config, policy, BASELINE_CLOCK_PAIR,
                                  "clock_pair", result);
}

int bench_futex_wait_mismatch(const struct bench_config *config,
                              enum bench_policy policy,
                              struct latency_result *result)
{
    return bench_syscall_baseline(config, policy,
                                  BASELINE_FUTEX_WAIT_MISMATCH,
                                  "futex_wait_mismatch", result);
}

int bench_futex_wake_empty(const struct bench_config *config,
                           enum bench_policy policy,
                           struct latency_result *result)
{
    return bench_syscall_baseline(config, policy, BASELINE_FUTEX_WAKE_EMPTY,
                                  "futex_wake_empty", result);
}
