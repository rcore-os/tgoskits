#define _GNU_SOURCE

#include "wakeup-latency-bench.h"

#include <errno.h>
#include <linux/reboot.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/reboot.h>
#include <unistd.h>

static const struct bench_config DEFAULT_CONFIG = {
    .warmup_samples = 1000,
    .handoff_samples = 20000,
    .timer_samples = 10000,
    .park_settle_ns = 50000,
    .timer_period_ns = 1000000,
    .sender_cpu = 0,
    .receiver_cpu = 1,
    .fifo_priority = 80,
};

static int configure_current(enum bench_policy policy, int cpu)
{
    int error = bench_pin_current_thread(cpu);
    if (error == 0) {
        error = bench_apply_policy(policy, DEFAULT_CONFIG.fifo_priority);
    }
    return error;
}

static int restore_normal_policy(void)
{
    return bench_apply_policy(BENCH_POLICY_OTHER,
                              DEFAULT_CONFIG.fifo_priority);
}

static int finish_result(struct latency_result *result)
{
    int error = restore_normal_policy();
    if (error != 0) {
        errno = error;
        perror("restore SCHED_OTHER before reporting");
        return -1;
    }
    bench_print_result(result);
    bench_release_result(result);
    return 0;
}

static int run_benchmark(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    if (sysconf(_SC_NPROCESSORS_ONLN) < 2) {
        fprintf(stderr, "need at least two online CPUs\n");
        return 1;
    }
    bench_print_metadata(&DEFAULT_CONFIG);

    const enum bench_policy policies[] = {
        BENCH_POLICY_OTHER,
        BENCH_POLICY_FIFO,
    };
    for (size_t policy_index = 0;
         policy_index < sizeof(policies) / sizeof(policies[0]);
         policy_index++) {
        enum bench_policy policy = policies[policy_index];
        int error = configure_current(policy, DEFAULT_CONFIG.sender_cpu);
        if (error != 0) {
            if (policy == BENCH_POLICY_FIFO &&
                (error == EPERM || error == ENOTSUP)) {
                printf("WAKEUP_LATENCY_SKIP {\"policy\":\"fifo\","
                       "\"errno\":%d}\n",
                       error);
                continue;
            }
            errno = error;
            perror("configure sender");
            return 1;
        }
        error = restore_normal_policy();
        if (error != 0) {
            errno = error;
            perror("restore policy after capability probe");
            return 1;
        }

        struct latency_result same_cpu;
        error = configure_current(policy, DEFAULT_CONFIG.sender_cpu);
        if (error != 0) {
            errno = error;
            perror("configure same-CPU sender");
            return 1;
        }
        if (bench_thread_handoff(&DEFAULT_CONFIG, policy, 1, &same_cpu) !=
            0) {
            perror("same-CPU thread handoff");
            return 1;
        }
        if (finish_result(&same_cpu) != 0) {
            return 1;
        }

        struct latency_result cross_cpu;
        error = configure_current(policy, DEFAULT_CONFIG.sender_cpu);
        if (error != 0) {
            errno = error;
            perror("configure cross-CPU sender");
            return 1;
        }
        if (bench_thread_handoff(&DEFAULT_CONFIG, policy, 0, &cross_cpu) !=
            0) {
            perror("cross-CPU thread handoff");
            return 1;
        }
        if (finish_result(&cross_cpu) != 0) {
            return 1;
        }

        struct latency_result process;
        error = configure_current(policy, DEFAULT_CONFIG.sender_cpu);
        if (error != 0) {
            errno = error;
            perror("configure process sender");
            return 1;
        }
        if (bench_process_handoff(&DEFAULT_CONFIG, policy, &process) != 0) {
            perror("cross-CPU process handoff");
            return 1;
        }
        if (finish_result(&process) != 0) {
            return 1;
        }

        error = configure_current(policy, DEFAULT_CONFIG.receiver_cpu);
        if (error != 0) {
            errno = error;
            perror("configure timer thread");
            return 1;
        }
        struct latency_result timer;
        if (bench_absolute_timer(&DEFAULT_CONFIG, policy, &timer) != 0) {
            perror("absolute timer wakeup");
            return 1;
        }
        if (finish_result(&timer) != 0) {
            return 1;
        }
    }

    printf("WAKEUP_LATENCY_PASSED\n");
    return 0;
}

int main(void)
{
#ifdef BENCH_INIT
    (void)mount("proc", "/proc", "proc", 0, NULL);
    printf("LINUX_RT_BENCH_BOOTED\n");
#endif
    int status = run_benchmark();
#ifdef BENCH_INIT
    sync();
    reboot(LINUX_REBOOT_CMD_POWER_OFF);
    for (;;) {
        pause();
    }
#endif
    return status;
}
