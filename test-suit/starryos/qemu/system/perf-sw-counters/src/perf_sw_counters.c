/*
 * perf_sw_counters.c -- PERF_TYPE_SOFTWARE events return real counts.
 *
 * `perf stat -- cmd` opens its default set with no `-e`: hardware cycles/
 * instructions plus five SOFTWARE events -- cpu-clock, task-clock,
 * context-switches, cpu-migrations, page-faults. Those used to dispatch to a BPF
 * stub with no readable count, so read(perf_fd) failed and every default row
 * printed `<not counted>`. This test opens all five as per-task counters on the
 * calling thread, runs a workload that exercises each, and asserts read(perf_fd)
 * succeeds with a sensible value.
 *
 * SUCCESS ==
 *     every event opens AND read(perf_fd) returns 8 bytes (a real u64 count)
 *   AND cpu-clock / task-clock / page-faults / context-switches are all > 0
 *       (the workload provably generates each).
 * cpu-migrations is only required to be READABLE (>= 0): a migration cannot be
 * forced deterministically under the test scheduler.
 *
 * If perf_event_open is not wired on this arch (ENOSYS) the test skips-as-pass.
 * On success exactly one line `STARRY_PERF_SW_COUNTERS_OK` is printed.
 *
 * All ABI structs are defined locally (no <linux/perf_event.h> dependency).
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#ifndef MADV_DONTNEED
#define MADV_DONTNEED 4
#endif

#define PERF_TYPE_SOFTWARE 1u

#define PERF_COUNT_SW_CPU_CLOCK 0ull
#define PERF_COUNT_SW_TASK_CLOCK 1ull
#define PERF_COUNT_SW_PAGE_FAULTS 2ull
#define PERF_COUNT_SW_CONTEXT_SWITCHES 3ull
#define PERF_COUNT_SW_CPU_MIGRATIONS 4ull

#ifndef PERF_EVENT_IOC_ENABLE
#define PERF_EVENT_IOC_ENABLE 0x2400u
#endif
#ifndef PERF_EVENT_IOC_DISABLE
#define PERF_EVENT_IOC_DISABLE 0x2401u
#endif
#ifndef PERF_EVENT_IOC_RESET
#define PERF_EVENT_IOC_RESET 0x2403u
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

#define PERF_ATTR_FLAG_DISABLED (1ull << 0)

#ifndef SYS_perf_event_open
#define SYS_perf_event_open 241
#endif

static long perf_event_open(struct perf_event_attr *attr, pid_t pid, int cpu,
                            int group_fd, unsigned long flags) {
    return syscall(SYS_perf_event_open, attr, pid, cpu, group_fd, flags);
}

static int open_sw(uint64_t config) {
    struct perf_event_attr attr;
    memset(&attr, 0, sizeof(attr));
    attr.type = PERF_TYPE_SOFTWARE;
    attr.size = (uint32_t)sizeof(attr);
    attr.config = config;
    attr.read_format = 0; /* read() returns just the u64 value */
    attr.flags = PERF_ATTR_FLAG_DISABLED;
    /* pid = 0 => this thread; cpu = -1 => any cpu (per-task). */
    long fd = perf_event_open(&attr, 0, -1, -1, 0ul);
    return (int)fd;
}

/* Workload that provably generates each software event. */
static volatile uint64_t g_sink;
static void workload(void) {
    /* task-clock / cpu-clock: burn some CPU (time-based, so TCG cycle
     * undercounting does not matter). */
    for (uint64_t i = 0; i < 20000000ull; i++) {
        g_sink += i * 2654435761ull + 1ull;
    }
    /* page-faults: fault in anonymous pages. Touch once (populates whether the
     * kernel maps lazily or eagerly), then MADV_DONTNEED to drop the pages and
     * re-touch -- the re-touch is a guaranteed demand-zero fault regardless of
     * the mmap population policy. */
    const size_t pages = 256;
    const size_t len = pages * 4096;
    void *p = mmap(NULL, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS,
                   -1, 0);
    if (p != MAP_FAILED) {
        volatile uint8_t *b = (volatile uint8_t *)p;
        for (size_t i = 0; i < pages; i++) {
            b[i * 4096] = (uint8_t)i;
        }
        (void)madvise(p, len, MADV_DONTNEED);
        for (size_t i = 0; i < pages; i++) {
            b[i * 4096] = (uint8_t)(i + 1);
        }
        (void)munmap(p, len);
    }
    /* context-switches: block repeatedly so the scheduler deschedules us. */
    for (int i = 0; i < 20; i++) {
        struct timespec ts = {0, 1000000}; /* 1 ms */
        (void)nanosleep(&ts, NULL);
    }
}

struct sw_event {
    const char *name;
    uint64_t config;
    int require_positive;
};

int main(void) {
    struct sw_event events[] = {
        {"cpu-clock", PERF_COUNT_SW_CPU_CLOCK, 1},
        {"task-clock", PERF_COUNT_SW_TASK_CLOCK, 1},
        {"page-faults", PERF_COUNT_SW_PAGE_FAULTS, 1},
        {"context-switches", PERF_COUNT_SW_CONTEXT_SWITCHES, 1},
        {"cpu-migrations", PERF_COUNT_SW_CPU_MIGRATIONS, 0},
    };
    const size_t n = sizeof(events) / sizeof(events[0]);
    int fds[8];

    for (size_t i = 0; i < n; i++) {
        fds[i] = open_sw(events[i].config);
        if (fds[i] < 0) {
            /* Not wired on this arch (ENOSYS) -> skip-as-pass. Any other errno on
             * the FIRST event is a real failure. */
            if (errno == ENOSYS && i == 0) {
                printf("STARRY_PERF_SW_COUNTERS skip: perf_event_open ENOSYS\n");
                printf("STARRY_PERF_SW_COUNTERS_OK\n");
                return 0;
            }
            printf("perf-sw-counters FAILED: open %s errno=%d\n", events[i].name,
                   errno);
            return 1;
        }
    }

    for (size_t i = 0; i < n; i++) {
        (void)ioctl(fds[i], PERF_EVENT_IOC_RESET, 0);
        (void)ioctl(fds[i], PERF_EVENT_IOC_ENABLE, 0);
    }
    workload();
    for (size_t i = 0; i < n; i++) {
        (void)ioctl(fds[i], PERF_EVENT_IOC_DISABLE, 0);
    }

    int rc = 0;
    for (size_t i = 0; i < n; i++) {
        uint64_t val = 0;
        ssize_t got = read(fds[i], &val, sizeof(val));
        if (got != (ssize_t)sizeof(val)) {
            printf("perf-sw-counters FAILED: read %s got=%zd errno=%d (counter "
                   "not readable -- still <not counted>)\n",
                   events[i].name, got, errno);
            rc = 1;
        } else {
            printf("STARRY_PERF_SW_COUNTERS %s=%llu\n", events[i].name,
                   (unsigned long long)val);
            if (events[i].require_positive && val == 0) {
                printf("perf-sw-counters FAILED: %s counted 0 (workload should "
                       "have generated events)\n",
                       events[i].name);
                rc = 1;
            }
        }
        close(fds[i]);
    }

    if (rc == 0) {
        printf("STARRY_PERF_SW_COUNTERS_OK\n");
    }
    return rc;
}
