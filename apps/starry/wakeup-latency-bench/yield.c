#define _GNU_SOURCE

#include "wakeup-latency-bench.h"

#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdlib.h>

struct yield_handoff_state {
    _Atomic uint32_t ready;
    _Atomic uint32_t turn;
    _Atomic uint32_t error;
    _Atomic uint64_t wake_timestamp_ns;
    size_t warmup_samples;
    size_t measured_samples;
    int cpu;
    int fifo_priority;
    enum bench_policy policy;
    uint64_t samples[];
};

static int publish_yield_error(struct yield_handoff_state *state, int error)
{
    uint32_t no_error = 0;
    (void)atomic_compare_exchange_strong_explicit(
        &state->error, &no_error, (uint32_t)error, memory_order_release,
        memory_order_relaxed);
    return -1;
}

static int yield_until(struct yield_handoff_state *state,
                       _Atomic uint32_t *word, uint32_t expected)
{
    while (atomic_load_explicit(word, memory_order_acquire) != expected) {
        uint32_t error =
            atomic_load_explicit(&state->error, memory_order_acquire);
        if (error != 0) {
            errno = (int)error;
            return -1;
        }
        if (sched_yield() != 0) {
            return publish_yield_error(state, errno);
        }
    }
    return 0;
}

static void *run_yield_receiver(void *argument)
{
    struct yield_handoff_state *state = argument;
    int error = bench_pin_current_thread(state->cpu);
    if (error == 0) {
        error = bench_apply_policy(state->policy, state->fifo_priority);
    }
    if (error != 0) {
        publish_yield_error(state, error);
        atomic_store_explicit(&state->ready, 1, memory_order_release);
        return (void *)(uintptr_t)1;
    }

    atomic_store_explicit(&state->ready, 1, memory_order_release);
    size_t total_samples = state->warmup_samples + state->measured_samples;
    for (size_t iteration = 0; iteration < total_samples; iteration++) {
        if (yield_until(state, &state->turn, 1) != 0) {
            return (void *)(uintptr_t)2;
        }
        uint64_t resumed_ns = bench_monotonic_ns();
        if (iteration >= state->warmup_samples) {
            uint64_t wake_ns = atomic_load_explicit(
                &state->wake_timestamp_ns, memory_order_acquire);
            state->samples[iteration - state->warmup_samples] =
                resumed_ns - wake_ns;
        }
        atomic_store_explicit(&state->turn, 0, memory_order_release);
        if (sched_yield() != 0) {
            publish_yield_error(state, errno);
            return (void *)(uintptr_t)3;
        }
    }
    return NULL;
}

int bench_yield_no_peer(const struct bench_config *config,
                        enum bench_policy policy,
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

    size_t total_samples = config->warmup_samples + config->handoff_samples;
    for (size_t iteration = 0; iteration < total_samples; iteration++) {
        uint64_t started = bench_monotonic_ns();
        int yield_result = sched_yield();
        int yield_errno = errno;
        uint64_t elapsed = bench_monotonic_ns() - started;
        if (yield_result != 0) {
            errno = yield_errno;
            free(samples);
            return -1;
        }
        if (iteration >= config->warmup_samples) {
            samples[iteration - config->warmup_samples] = elapsed;
        }
    }

    *result = (struct latency_result) {
        .case_name = "sched_yield_no_peer",
        .policy = policy,
        .samples = samples,
        .sample_count = config->handoff_samples,
        .attempted_samples = config->handoff_samples,
        .storage = samples,
        .storage_size = storage_size,
    };
    return 0;
}

int bench_yield_handoff(const struct bench_config *config,
                        enum bench_policy policy,
                        struct latency_result *result)
{
    if (config->handoff_samples >
        (SIZE_MAX - sizeof(struct yield_handoff_state)) / sizeof(uint64_t)) {
        errno = EOVERFLOW;
        return -1;
    }
    size_t storage_size = sizeof(struct yield_handoff_state) +
                          config->handoff_samples * sizeof(uint64_t);
    struct yield_handoff_state *state = calloc(1, storage_size);
    if (state == NULL) {
        return -1;
    }
    state->warmup_samples = config->warmup_samples;
    state->measured_samples = config->handoff_samples;
    state->cpu = config->sender_cpu;
    state->fifo_priority = config->fifo_priority;
    state->policy = policy;
    bench_prefault_samples(state->samples, config->handoff_samples);

    pthread_t receiver;
    int error = pthread_create(&receiver, NULL, run_yield_receiver, state);
    if (error != 0) {
        errno = error;
        free(state);
        return -1;
    }

    int sender_result = yield_until(state, &state->ready, 1);
    size_t total_samples = config->warmup_samples + config->handoff_samples;
    for (size_t iteration = 0;
         sender_result == 0 && iteration < total_samples; iteration++) {
        atomic_store_explicit(&state->wake_timestamp_ns,
                              bench_monotonic_ns(), memory_order_release);
        atomic_store_explicit(&state->turn, 1, memory_order_release);
        if (sched_yield() != 0 ||
            yield_until(state, &state->turn, 0) != 0) {
            sender_result = publish_yield_error(state, errno);
        }
    }

    void *receiver_result = NULL;
    error = pthread_join(receiver, &receiver_result);
    uint32_t published_error =
        atomic_load_explicit(&state->error, memory_order_acquire);
    if (sender_result != 0 || error != 0 || receiver_result != NULL ||
        published_error != 0) {
        if (published_error != 0) {
            errno = (int)published_error;
        } else if (error != 0) {
            errno = error;
        } else if (errno == 0) {
            errno = EPROTO;
        }
        free(state);
        return -1;
    }

    *result = (struct latency_result) {
        .case_name = "sched_yield_handoff",
        .policy = policy,
        .samples = state->samples,
        .sample_count = config->handoff_samples,
        .attempted_samples = config->handoff_samples,
        .storage = state,
        .storage_size = storage_size,
    };
    return 0;
}
