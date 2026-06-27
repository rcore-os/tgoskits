/*
 * perf-hw-smp-migrate -- a per-task hardware counter must keep counting as the
 * monitored task migrates across cores (per-CPU pools + per-slice allocation).
 *
 * Parent opens a per-task RAW 0x11 (ARM CPU_CYCLES on a programmable counter)
 * event on the child (pid>0), enable_on_exec NOT used -- the event is enabled by
 * the parent right after open and counts from the child's next sched-in. The
 * child busy-loops while pinning itself to cpu 0,1,2,3 in turn
 * (sched_setaffinity), so the counter is reserved/released on each core's own
 * per-CPU pool per slice. Parent waits, reads the counter.
 *
 * SUCCESS: fd>=0, read()==24 bytes (read_format=TIMING), value>0 (counting
 * survived migration), time_enabled>0, and time_running <= time_enabled.
 *
 * Before S2 (global allocator + cross-core live read) this either lost counting
 * off the opening core or read a different core's counter; with per-CPU pools +
 * per-slice allocation + the read_values cross-core guard it counts correctly.
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
/* read_format = TOTAL_TIME_ENABLED|TOTAL_TIME_RUNNING (==3) -> 3 u64 on read. */
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

static void busy_migrate(void) {
    volatile uint64_t spin = 0;
    for (int cpu = 0; cpu < 4; cpu++) {
        cpu_set_t set;
        CPU_ZERO(&set);
        CPU_SET(cpu, &set);
        (void)sched_setaffinity(0, sizeof(set), &set);
        for (uint64_t i = 0; i < 8000000ull; i++) {
            spin += i;
        }
    }
    _exit(0);
}

int main(void) {
    struct perf_event_attr attr;
    for (size_t i = 0; i < sizeof(attr); i++) {
        ((volatile unsigned char *)&attr)[i] = 0;
    }

    pid_t child = fork();
    if (child == 0) {
        busy_migrate();
    }
    if (child < 0) {
        printf("smp-migrate FAILED: fork errno=%d\n", errno);
        return 1;
    }

    attr.type = PERF_TYPE_RAW;
    attr.config = ARM_CPU_CYCLES;
    attr.size = sizeof(attr);
    attr.read_format = PERF_FORMAT_TIMING;
    attr.flags = PERF_ATTR_DISABLED;

    long fd = peo(&attr, child, -1, -1, 0);
    if (fd < 0) {
        printf("smp-migrate FAILED: perf_event_open errno=%d\n", errno);
        return 1;
    }
    (void)ioctl((int)fd, PERF_IOC_ENABLE, 0);

    int status = 0;
    waitpid(child, &status, 0);
    (void)ioctl((int)fd, PERF_IOC_DISABLE, 0);

    uint64_t buf[3] = {0, 0, 0};
    ssize_t n = read((int)fd, buf, sizeof(buf));
    printf("STARRY_SMP_MIGRATE value=%llu enabled=%llu running=%llu n=%lld\n",
           (unsigned long long)buf[0], (unsigned long long)buf[1],
           (unsigned long long)buf[2], (long long)n);

    int ok = 1;
    if (n != 24) {
        printf("smp-migrate FAILED: read %lld != 24\n", (long long)n);
        ok = 0;
    }
    if (buf[0] == 0) {
        printf("smp-migrate FAILED: value 0 (lost counting on migration)\n");
        ok = 0;
    }
    if (buf[1] == 0) {
        printf("smp-migrate FAILED: time_enabled 0\n");
        ok = 0;
    }
    if (buf[2] > buf[1]) {
        printf("smp-migrate FAILED: time_running > time_enabled\n");
        ok = 0;
    }
    close((int)fd);

    if (ok) {
        printf("STARRY_SMP_MIGRATE_OK\n");
        return 0;
    }
    return 1;
}
