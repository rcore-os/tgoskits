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

enum bench_case {
    BENCH_CASE_ALL,
    BENCH_CASE_THREAD_FUTEX_SAME_CPU,
    BENCH_CASE_THREAD_FUTEX_CROSS_CPU,
    BENCH_CASE_PROCESS_FUTEX_CROSS_CPU,
    BENCH_CASE_ABSOLUTE_TIMER_SAME_CPU,
};

struct bench_selection {
    enum bench_case bench_case;
    int run_other;
    int run_fifo;
};

static void print_usage(const char *program)
{
    fprintf(stderr,
            "usage: %s [--policy all|other|fifo] "
            "[--case all|thread_futex_same_cpu|thread_futex_cross_cpu|"
            "process_futex_cross_cpu|absolute_timer_same_cpu]\n",
            program);
}

static int parse_policy(const char *value, struct bench_selection *selection)
{
    if (strcmp(value, "all") == 0) {
        selection->run_other = 1;
        selection->run_fifo = 1;
    } else if (strcmp(value, "other") == 0) {
        selection->run_other = 1;
        selection->run_fifo = 0;
    } else if (strcmp(value, "fifo") == 0) {
        selection->run_other = 0;
        selection->run_fifo = 1;
    } else {
        return -1;
    }
    return 0;
}

static int parse_case(const char *value, struct bench_selection *selection)
{
    if (strcmp(value, "all") == 0) {
        selection->bench_case = BENCH_CASE_ALL;
    } else if (strcmp(value, "thread_futex_same_cpu") == 0) {
        selection->bench_case = BENCH_CASE_THREAD_FUTEX_SAME_CPU;
    } else if (strcmp(value, "thread_futex_cross_cpu") == 0) {
        selection->bench_case = BENCH_CASE_THREAD_FUTEX_CROSS_CPU;
    } else if (strcmp(value, "process_futex_cross_cpu") == 0) {
        selection->bench_case = BENCH_CASE_PROCESS_FUTEX_CROSS_CPU;
    } else if (strcmp(value, "absolute_timer_same_cpu") == 0) {
        selection->bench_case = BENCH_CASE_ABSOLUTE_TIMER_SAME_CPU;
    } else {
        return -1;
    }
    return 0;
}

static int parse_selection(int argc, char **argv,
                           struct bench_selection *selection)
{
    *selection = (struct bench_selection) {
        .bench_case = BENCH_CASE_ALL,
        .run_other = 1,
        .run_fifo = 1,
    };

    for (int argument = 1; argument < argc; argument++) {
        if (strcmp(argv[argument], "--help") == 0) {
            print_usage(argv[0]);
            return 1;
        }
        if (argument + 1 >= argc) {
            fprintf(stderr, "missing value for %s\n", argv[argument]);
            return -1;
        }
        const char *value = argv[++argument];
        if (strcmp(argv[argument - 1], "--policy") == 0) {
            if (parse_policy(value, selection) != 0) {
                fprintf(stderr, "invalid policy: %s\n", value);
                return -1;
            }
        } else if (strcmp(argv[argument - 1], "--case") == 0) {
            if (parse_case(value, selection) != 0) {
                fprintf(stderr, "invalid case: %s\n", value);
                return -1;
            }
        } else {
            fprintf(stderr, "unknown option: %s\n", argv[argument - 1]);
            return -1;
        }
    }
    return 0;
}

static int policy_selected(const struct bench_selection *selection,
                           enum bench_policy policy)
{
    return policy == BENCH_POLICY_OTHER ? selection->run_other
                                        : selection->run_fifo;
}

static int case_selected(const struct bench_selection *selection,
                         enum bench_case bench_case)
{
    return selection->bench_case == BENCH_CASE_ALL ||
           selection->bench_case == bench_case;
}

static void print_case_marker(const char *phase, const char *case_name,
                              enum bench_policy policy)
{
    printf("WAKEUP_LATENCY_CASE_%s case=%s policy=%s\n", phase, case_name,
           bench_policy_name(policy));
}

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

static int run_benchmark(const struct bench_selection *selection)
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
        if (!policy_selected(selection, policy)) {
            continue;
        }
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

        if (case_selected(selection, BENCH_CASE_THREAD_FUTEX_SAME_CPU)) {
            print_case_marker("START", "thread_futex_same_cpu", policy);
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
            print_case_marker("DONE", "thread_futex_same_cpu", policy);
        }

        if (case_selected(selection, BENCH_CASE_THREAD_FUTEX_CROSS_CPU)) {
            print_case_marker("START", "thread_futex_cross_cpu", policy);
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
            print_case_marker("DONE", "thread_futex_cross_cpu", policy);
        }

        if (case_selected(selection, BENCH_CASE_PROCESS_FUTEX_CROSS_CPU)) {
            print_case_marker("START", "process_futex_cross_cpu", policy);
            struct latency_result process;
            error = configure_current(policy, DEFAULT_CONFIG.sender_cpu);
            if (error != 0) {
                errno = error;
                perror("configure process sender");
                return 1;
            }
            if (bench_process_handoff(&DEFAULT_CONFIG, policy, &process) !=
                0) {
                perror("cross-CPU process handoff");
                return 1;
            }
            if (finish_result(&process) != 0) {
                return 1;
            }
            print_case_marker("DONE", "process_futex_cross_cpu", policy);
        }

        if (case_selected(selection, BENCH_CASE_ABSOLUTE_TIMER_SAME_CPU)) {
            print_case_marker("START", "absolute_timer_same_cpu", policy);
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
            print_case_marker("DONE", "absolute_timer_same_cpu", policy);
        }
    }

    printf("WAKEUP_LATENCY_PASSED\n");
    return 0;
}

int main(int argc, char **argv)
{
#ifdef BENCH_INIT
    (void)mount("proc", "/proc", "proc", 0, NULL);
    printf("LINUX_RT_BENCH_BOOTED\n");
#endif
    struct bench_selection selection;
    int parse_result = parse_selection(argc, argv, &selection);
    int status;
    if (parse_result > 0) {
        status = 0;
    } else if (parse_result < 0) {
        print_usage(argv[0]);
        status = 2;
    } else {
        status = run_benchmark(&selection);
    }
#ifdef BENCH_INIT
    sync();
    reboot(LINUX_REBOOT_CMD_POWER_OFF);
    for (;;) {
        pause();
    }
#endif
    return status;
}
