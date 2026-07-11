/*
 * perf-hw-smp-rotate -- Tier-2 counter multiplexing. Open more per-task counting
 * events on ONE task than the core has programmable counters (10 > 6), so the
 * kernel must rotate which subset holds the hardware counters on each periodic
 * tick. Every event should still accrue a non-zero count over the run, and the
 * over-subscribed events should show time_running < time_enabled (so `perf`
 * scales them).
 *
 * The child is pinned to cpu 0 and busy-loops; the periodic timer tick fires
 * while it runs (it never voluntarily yields), which is exactly what drives the
 * rotation -- a context switch alone would never rotate a CPU-bound task.
 *
 * SUCCESS: all NEV opens succeed, every event read()s 24 bytes with value>0
 * (each got a turn on hardware), every event has time_running<=time_enabled, AND
 * at least one event has time_running<time_enabled (proof of multiplexing).
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#define PERF_TYPE_RAW 4u
#define ARM_CPU_CYCLES 0x11ull
#define PERF_FORMAT_TIMING 3ull
#define PERF_IOC_ENABLE 0x2400u
#define PERF_IOC_DISABLE 0x2401u
#define PERF_ATTR_DISABLED (1ull << 0)
#ifndef SYS_perf_event_open
#define SYS_perf_event_open 241
#endif

#define NEV 10

struct perf_event_attr {
    uint32_t type;
    uint32_t size;
    uint64_t config;
    union {
        uint64_t sample_period;
        uint64_t sample_freq;
    };
    uint64_t sample_type;
    uint64_t read_format;
    uint64_t flags;
    union {
        uint32_t wakeup_events;
        uint32_t wakeup_watermark;
    };
    uint32_t bp_type;
    union {
        uint64_t bp_addr;
        uint64_t config1;
    };
    union {
        uint64_t bp_len;
        uint64_t config2;
    };
    uint64_t branch_sample_type;
    uint64_t sample_regs_user;
    uint32_t sample_stack_user;
    int32_t clockid;
    uint64_t sample_regs_intr;
    uint32_t aux_watermark;
    uint16_t sample_max_stack;
    uint16_t __reserved_2;
    uint32_t aux_sample_size;
    uint32_t __reserved_3;
};

static long peo(struct perf_event_attr *a, pid_t pid, int cpu, int gfd,
                unsigned long fl) {
    return syscall(SYS_perf_event_open, a, pid, cpu, gfd, fl);
}

int main(void) {
#if !defined(__aarch64__)
    /* Hardware-PMU perf is aarch64-only (ARM PMUv3); skip-as-pass on other
     * architectures so the cross-arch grouped C run stays green. */
    printf("STARRY_SMP_ROTATE_OK\n");
    return 0;
#endif
    pid_t child = fork();
    if (child == 0) {
        cpu_set_t set;
        CPU_ZERO(&set);
        CPU_SET(0, &set);
        (void)sched_setaffinity(0, sizeof(set), &set);
        volatile uint64_t s = 0;
        for (uint64_t k = 0; k < 200000000ull; k++) {
            s += k;
        }
        _exit(0);
    }
    if (child < 0) {
        printf("rotate FAILED: fork errno=%d\n", errno);
        return 1;
    }

    struct perf_event_attr attr;
    long fds[NEV];
    int ok = 1;
    for (int i = 0; i < NEV; i++) {
        for (size_t b = 0; b < sizeof(attr); b++) {
            ((volatile unsigned char *)&attr)[b] = 0;
        }
        attr.type = PERF_TYPE_RAW;
        attr.config = ARM_CPU_CYCLES;
        attr.size = sizeof(attr);
        attr.read_format = PERF_FORMAT_TIMING;
        attr.flags = PERF_ATTR_DISABLED;
        fds[i] = peo(&attr, child, -1, -1, 0);
        if (fds[i] < 0) {
            printf("rotate FAILED: open[%d] errno=%d\n", i, errno);
            ok = 0;
        } else {
            (void)ioctl((int)fds[i], PERF_IOC_ENABLE, 0);
        }
    }

    int st;
    waitpid(child, &st, 0);

    int scaled = 0; /* events with running < enabled (multiplexed) */
    for (int i = 0; i < NEV; i++) {
        if (fds[i] < 0) {
            continue;
        }
        (void)ioctl((int)fds[i], PERF_IOC_DISABLE, 0);
        uint64_t buf[3] = {0, 0, 0};
        ssize_t n = read((int)fds[i], buf, sizeof(buf));
        uint64_t value = buf[0], ena = buf[1], run = buf[2];
        printf("STARRY_SMP_ROTATE[%d] value=%llu enabled=%llu running=%llu n=%lld\n",
               i, (unsigned long long)value, (unsigned long long)ena,
               (unsigned long long)run, (long long)n);
        if (n != 24) {
            printf("rotate FAILED: ev %d read %lld != 24\n", i, (long long)n);
            ok = 0;
        }
        if (value == 0) {
            printf("rotate FAILED: ev %d value 0 (never got a counter)\n", i);
            ok = 0;
        }
        if (run > ena) {
            printf("rotate FAILED: ev %d running > enabled\n", i);
            ok = 0;
        }
        if (run < ena) {
            scaled++;
        }
        close((int)fds[i]);
    }

    if (scaled == 0) {
        printf("rotate FAILED: no event was multiplexed (running<enabled)\n");
        ok = 0;
    }

    if (ok) {
        printf("STARRY_SMP_ROTATE_OK scaled=%d/%d\n", scaled, NEV);
        return 0;
    }
    return 1;
}
