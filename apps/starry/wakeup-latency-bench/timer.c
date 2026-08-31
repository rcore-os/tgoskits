#define _GNU_SOURCE

#include "wakeup-latency-bench.h"

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>

static struct timespec ns_to_timespec(uint64_t nanoseconds)
{
    return (struct timespec) {
        .tv_sec = (time_t)(nanoseconds / 1000000000ULL),
        .tv_nsec = (long)(nanoseconds % 1000000000ULL),
    };
}

static int sleep_until(uint64_t deadline_ns)
{
    struct timespec deadline = ns_to_timespec(deadline_ns);
    for (;;) {
        int error =
            clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &deadline, NULL);
        if (error == 0) {
            return 0;
        }
        if (error != EINTR) {
            errno = error;
            return -1;
        }
    }
}

int bench_absolute_timer(const struct bench_config *config,
                         enum bench_policy policy,
                         struct latency_result *result)
{
    uint64_t *samples =
        calloc(config->timer_samples, sizeof(*samples));
    if (samples == NULL) {
        return -1;
    }
    bench_prefault_samples(samples, config->timer_samples);

    uint64_t deadline_ns = bench_monotonic_ns() + 10000000ULL;
    uint64_t missed_deadlines = 0;
    size_t total_samples = config->warmup_samples + config->timer_samples;
    for (size_t sample = 0; sample < total_samples; sample++) {
        deadline_ns += config->timer_period_ns;
        if (sleep_until(deadline_ns) != 0) {
            free(samples);
            return -1;
        }
        uint64_t resumed_ns = bench_monotonic_ns();
        uint64_t lateness_ns = resumed_ns > deadline_ns
                                   ? resumed_ns - deadline_ns
                                   : 0;
        if (sample >= config->warmup_samples) {
            samples[sample - config->warmup_samples] = lateness_ns;
            if (lateness_ns >= config->timer_period_ns) {
                missed_deadlines += lateness_ns / config->timer_period_ns;
            }
        }
    }

    *result = (struct latency_result) {
        .case_name = "absolute_timer_same_cpu",
        .policy = policy,
        .samples = samples,
        .sample_count = config->timer_samples,
        .attempted_samples = config->timer_samples,
        .missed_deadlines = missed_deadlines,
        .storage = samples,
    };
    return 0;
}
