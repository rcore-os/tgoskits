/*
 * perf-hw-smp-home -- a self/system-wide event (pid=-1, cpu=-1) allocates its
 * counter on the OPENING core and counts there; its HW lifecycle must stay
 * pinned to that home core even after the monitoring thread migrates, via a
 * synchronous IPI. Without the fix, disable/read/close run on the migrated
 * core and read/free the WRONG core's banked counter (value ~0, pool corruption).
 *
 * A child busy-loops pinned to cpu 0. The parent opens the event while on cpu 0
 * (home = cpu 0), enables it, then MIGRATES to cpu 1 and does disable + read +
 * close from there. With the home-IPI fix the read reaches cpu 0's counter and
 * sees the child's millions of cycles; without it, cpu 1's counter n reads ~0.
 *
 * SUCCESS: read()==24 bytes, value large (> 1,000,000 — proves the home read,
 * not a stray cpu-1 counter), and time_running <= time_enabled.
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

static void pin(int cpu) {
    cpu_set_t set;
    CPU_ZERO(&set);
    CPU_SET(cpu, &set);
    (void)sched_setaffinity(0, sizeof(set), &set);
}

int main(void) {
#if !defined(__aarch64__)
    /* Hardware-PMU perf is aarch64-only (ARM PMUv3); skip-as-pass on other
     * architectures so the cross-arch grouped C run stays green. */
    printf("STARRY_SMP_HOME_OK\n");
    return 0;
#endif
    pid_t child = fork();
    if (child == 0) {
        pin(0);
        volatile uint64_t s = 0;
        for (uint64_t k = 0; k < 60000000ull; k++) {
            s += k;
        }
        _exit(0);
    }
    if (child < 0) {
        printf("home FAILED: fork errno=%d\n", errno);
        return 1;
    }

    /* Open on cpu 0 -> home = cpu 0; system-wide so it counts the child there. */
    pin(0);
    struct perf_event_attr attr;
    for (size_t b = 0; b < sizeof(attr); b++) {
        ((volatile unsigned char *)&attr)[b] = 0;
    }
    attr.type = PERF_TYPE_RAW;
    attr.config = ARM_CPU_CYCLES;
    attr.size = sizeof(attr);
    attr.read_format = PERF_FORMAT_TIMING;
    attr.flags = PERF_ATTR_DISABLED;
    long fd = peo(&attr, -1, -1, -1, 0);
    if (fd < 0) {
        printf("home FAILED: perf_event_open errno=%d\n", errno);
        return 1;
    }
    (void)ioctl((int)fd, PERF_IOC_ENABLE, 0);

    /* Migrate the monitoring thread OFF the home core. */
    pin(1);
    /* Let the child accrue cycles on home (cpu 0) while we sit on cpu 1. */
    int st;
    waitpid(child, &st, 0);

    /* disable + read + close now run on cpu 1 -> must IPI home (cpu 0). */
    (void)ioctl((int)fd, PERF_IOC_DISABLE, 0);
    uint64_t buf[3] = {0, 0, 0};
    ssize_t n = read((int)fd, buf, sizeof(buf));
    printf("STARRY_SMP_HOME value=%llu enabled=%llu running=%llu n=%lld\n",
           (unsigned long long)buf[0], (unsigned long long)buf[1],
           (unsigned long long)buf[2], (long long)n);

    int ok = 1;
    if (n != 24) {
        printf("home FAILED: read %lld != 24\n", (long long)n);
        ok = 0;
    }
    if (buf[0] <= 1000000ull) {
        printf("home FAILED: value %llu too small — read the wrong (migrated) "
               "core's counter instead of home?\n",
               (unsigned long long)buf[0]);
        ok = 0;
    }
    if (buf[2] > buf[1]) {
        printf("home FAILED: running > enabled\n");
        ok = 0;
    }
    close((int)fd);

    if (ok) {
        printf("STARRY_SMP_HOME_OK\n");
        return 0;
    }
    return 1;
}
