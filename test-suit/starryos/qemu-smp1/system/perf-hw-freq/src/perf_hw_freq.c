/*
 * perf_hw_freq.c -- frequency-mode sampling (`perf record -F`) test.
 *
 * A frequency-mode event (`attr.freq = 1`, `sample_freq = N`) asks the kernel to
 * sample at ~N Hz rather than every fixed `N` events. The kernel can't know the
 * event rate up front, so it starts at an estimate and adapts the period after
 * each overflow toward the target rate (Linux `perf_adjust_period`).
 *
 * This test opens a system-wide frequency-mode CPU_CYCLES event with
 * `sample_type = IP|PERIOD` (so each record carries the period in effect), runs a
 * busy loop, then walks the ring and checks that (a) samples were captured and
 * (b) the per-record period is NOT pinned at the initial estimate — i.e. the
 * adaptive control loop actually ran. (Exact rate convergence is timing-
 * dependent and not asserted under TCG.)
 *
 * SUCCESS == >= 2 samples AND at least one record's period differs from the
 * initial estimate. Prints the single sentinel STARRY_PERF_FREQ_OK.
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
#include <unistd.h>

#ifndef PERF_TYPE_RAW
#define PERF_TYPE_RAW 4u
#endif
#ifndef ARM_PMU_EVT_CPU_CYCLES
#define ARM_PMU_EVT_CPU_CYCLES 0x11ull
#endif
#define PERF_SAMPLE_IP (1ull << 0)
#define PERF_SAMPLE_PERIOD (1ull << 8)
#define SAMPLE_FREQ 2000ull
/* Kernel's initial estimate: 1e9 / freq (assumes ~1 GHz; see sampling.rs). */
#define INITIAL_PERIOD (1000000000ull / SAMPLE_FREQ)

#ifndef PERF_EVENT_IOC_ENABLE
#define PERF_EVENT_IOC_ENABLE 0x2400u
#endif
#ifndef PERF_EVENT_IOC_DISABLE
#define PERF_EVENT_IOC_DISABLE 0x2401u
#endif
#define PERF_ATTR_FLAG_DISABLED (1ull << 0)
#define PERF_ATTR_FLAG_FREQ (1ull << 10)
#ifndef PERF_RECORD_SAMPLE
#define PERF_RECORD_SAMPLE 9u
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

struct perf_event_mmap_page {
    uint32_t version;
    uint32_t compat_version;
    uint32_t lock;
    uint32_t index;
    int64_t offset;
    uint64_t time_enabled;
    uint64_t time_running;
    uint64_t capabilities;
    uint16_t pmc_width;
    uint16_t time_shift;
    uint32_t time_mult;
    uint64_t time_offset;
    uint64_t time_zero;
    uint32_t size;
    uint32_t __reserved_1;
    uint64_t time_cycles;
    uint64_t time_mask;
    uint8_t __reserved[928];
    uint64_t data_head;
    uint64_t data_tail;
    uint64_t data_offset;
    uint64_t data_size;
};

struct perf_event_header {
    uint32_t type;
    uint16_t misc;
    uint16_t size;
};

#ifndef SYS_perf_event_open
#define SYS_perf_event_open 241
#endif
#define PERF_MMAP_DATA_PAGES 8u
#define PERF_MMAP_TOTAL_BYTES ((size_t)(1u + PERF_MMAP_DATA_PAGES) * 4096u)

static long perf_event_open(struct perf_event_attr *attr, pid_t pid, int cpu,
                            int group_fd, unsigned long flags) {
    return syscall(SYS_perf_event_open, attr, pid, cpu, group_fd, flags);
}

static int fail(const char *reason) {
    printf("perf-freq FAILED: %s\n", reason);
    return 1;
}

int main(void) {
    struct perf_event_attr attr;
    memset(&attr, 0, sizeof(attr));
    attr.type = PERF_TYPE_RAW;
    attr.config = ARM_PMU_EVT_CPU_CYCLES;
    attr.size = (uint32_t)sizeof(attr);
    attr.sample_freq = SAMPLE_FREQ;
    attr.sample_type = PERF_SAMPLE_IP | PERF_SAMPLE_PERIOD;
    attr.flags = PERF_ATTR_FLAG_DISABLED | PERF_ATTR_FLAG_FREQ;

    /* System-wide on cpu0 (single-core under test): samples whatever runs. */
    long fd = perf_event_open(&attr, -1, 0, -1, 0ul);
    if (fd < 0) {
        char msg[96];
        snprintf(msg, sizeof(msg), "perf_event_open errno=%d", errno);
        return fail(msg);
    }
    int efd = (int)fd;

    void *base = mmap(NULL, PERF_MMAP_TOTAL_BYTES, PROT_READ | PROT_WRITE,
                      MAP_SHARED, efd, 0);
    if (base == MAP_FAILED) {
        close(efd);
        return fail("mmap ring");
    }
    struct perf_event_mmap_page *meta = (struct perf_event_mmap_page *)base;

    if (ioctl(efd, PERF_EVENT_IOC_ENABLE, 0) != 0) {
        munmap(base, PERF_MMAP_TOTAL_BYTES);
        close(efd);
        return fail("ioctl(ENABLE)");
    }
    volatile uint64_t spin = 0;
    for (uint64_t i = 0; i < 80000000ull; i++) {
        spin += i;
    }
    (void)ioctl(efd, PERF_EVENT_IOC_DISABLE, 0);

    uint64_t data_head = meta->data_head;
    __sync_synchronize();
    uint64_t data_tail = meta->data_tail;
    uint64_t data_offset = meta->data_offset;
    uint64_t data_size = meta->data_size;
    const uint8_t *data_base = (const uint8_t *)base + data_offset;

    uint64_t sample_count = 0;
    uint64_t first_period = 0;
    int saw_non_initial = 0;
    uint64_t off = data_tail;
    while (off < data_head && data_size != 0) {
        uint64_t rel = off % data_size;
        struct perf_event_header hdr;
        for (size_t b = 0; b < sizeof(hdr); b++) {
            ((uint8_t *)&hdr)[b] = data_base[(rel + b) % data_size];
        }
        if (hdr.size == 0 || off + hdr.size > data_head) {
            break;
        }
        if (hdr.type == PERF_RECORD_SAMPLE) {
            /* body: IP (8) then PERIOD (8). */
            uint64_t period = 0;
            uint64_t pbody = rel + sizeof(hdr) + 8;
            for (size_t b = 0; b < sizeof(period); b++) {
                ((uint8_t *)&period)[b] = data_base[(pbody + b) % data_size];
            }
            if (sample_count == 0) {
                first_period = period;
            }
            if (period != INITIAL_PERIOD && period != 0) {
                saw_non_initial = 1;
            }
            sample_count++;
        }
        off += hdr.size;
    }

    printf("STARRY_PERF_FREQ samples=%llu first_period=%llu initial=%llu "
           "adapted=%d\n",
           (unsigned long long)sample_count, (unsigned long long)first_period,
           (unsigned long long)INITIAL_PERIOD, saw_non_initial);

    int rc = 0;
    if (sample_count < 2) {
        rc = fail("fewer than 2 samples captured");
    } else if (!saw_non_initial) {
        rc = fail("period never adapted off the initial estimate");
    }

    munmap(base, PERF_MMAP_TOTAL_BYTES);
    close(efd);
    if (rc == 0) {
        printf("STARRY_PERF_FREQ_OK\n");
        return 0;
    }
    return rc;
}
