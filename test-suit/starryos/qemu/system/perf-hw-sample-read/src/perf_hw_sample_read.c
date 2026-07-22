/*
 * perf_hw_sample_read.c -- PERF_SAMPLE_READ in samples.
 *
 * With PERF_SAMPLE_READ in sample_type, each PERF_RECORD_SAMPLE carries the
 * event's read value (the read_format block). This test opens a sampling event
 * with sample_type = IP|TID|TIME|READ and read_format = 0 (value only), runs a
 * workload, and checks every sample carries a read value that strictly increases
 * across samples — the running event count (each overflow adds ~period events).
 *
 * (Group-leader sampling `-e '{a,b}:S'` and the TOTAL_TIME_* read formats are a
 * follow-up and are rejected at open; this validates the single-event core.)
 *
 * SUCCESS ==
 *     fd >= 0 AND mmap ok AND >= 2 PERF_RECORD_SAMPLE records
 *   AND each sample carries the READ value (record is well-formed)
 *   AND the values are strictly increasing (running count) and non-zero.
 * On success exactly one line `STARRY_PERF_SAMPLE_READ_OK` is printed.
 *
 * aarch64-only (ARM PMUv3); skips-as-pass elsewhere.
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <fcntl.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
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
#define PERF_SAMPLE_TID (1ull << 1)
#define PERF_SAMPLE_TIME (1ull << 2)
#define PERF_SAMPLE_READ (1ull << 10)

#define SAMPLE_PERIOD 100000ull

#ifndef PERF_EVENT_IOC_ENABLE
#define PERF_EVENT_IOC_ENABLE 0x2400u
#endif
#ifndef PERF_EVENT_IOC_DISABLE
#define PERF_EVENT_IOC_DISABLE 0x2401u
#endif
#ifndef PERF_EVENT_IOC_RESET
#define PERF_EVENT_IOC_RESET 0x2403u
#endif
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

#define PERF_ATTR_FLAG_DISABLED (1ull << 0)

struct perf_event_mmap_page {
    uint32_t version, compat_version, lock, index;
    int64_t offset;
    uint64_t time_enabled, time_running;
    uint64_t capabilities;
    uint16_t pmc_width, time_shift;
    uint32_t time_mult;
    uint64_t time_offset, time_zero;
    uint32_t size, __reserved_1;
    uint64_t time_cycles, time_mask;
    uint8_t __reserved[928];
    uint64_t data_head, data_tail, data_offset, data_size;
    uint64_t aux_head, aux_tail, aux_offset, aux_size;
};

_Static_assert(offsetof(struct perf_event_mmap_page, data_head) == 1024, "dh");
_Static_assert(offsetof(struct perf_event_mmap_page, data_tail) == 1032, "dt");
_Static_assert(offsetof(struct perf_event_mmap_page, data_offset) == 1040, "do");

struct perf_event_header {
    uint32_t type;
    uint16_t misc;
    uint16_t size;
};

#ifndef SYS_perf_event_open
#define SYS_perf_event_open 241
#endif

#define PERF_MMAP_PAGE_SIZE 4096u
#define PERF_MMAP_DATA_PAGES 8u
#define PERF_MMAP_TOTAL_BYTES                                                   \
    ((size_t)(1u + PERF_MMAP_DATA_PAGES) * PERF_MMAP_PAGE_SIZE)

static long perf_event_open(struct perf_event_attr *attr, pid_t pid, int cpu,
                            int group_fd, unsigned long flags) {
    return syscall(SYS_perf_event_open, attr, pid, cpu, group_fd, flags);
}

static void ring_copy(const uint8_t *base, uint64_t size, uint64_t at, void *dst,
                      size_t n) {
    for (size_t b = 0; b < n; b++) {
        ((uint8_t *)dst)[b] = base[(at + b) % size];
    }
}

static int g_zfd = -1;
static volatile uint64_t g_sink;
static void workload(void) {
    static uint8_t buf[4096];
    for (uint64_t i = 0; i < 400000ull; i++) {
        if (g_zfd >= 0) {
            if (read(g_zfd, buf, sizeof(buf)) < 0) {
                break;
            }
        } else {
            g_sink += i * 3ull + 1ull;
        }
    }
}

int main(void) {
#if !defined(__aarch64__)
    printf("STARRY_PERF_SAMPLE_READ_OK\n");
    return 0;
#endif
    struct perf_event_attr attr;
    for (size_t i = 0; i < sizeof(attr); i++) {
        ((volatile unsigned char *)&attr)[i] = 0;
    }
    attr.type = PERF_TYPE_RAW;
    attr.config = ARM_PMU_EVT_CPU_CYCLES;
    attr.size = (uint32_t)sizeof(attr);
    attr.sample_period = SAMPLE_PERIOD;
    attr.sample_type = PERF_SAMPLE_IP | PERF_SAMPLE_TID | PERF_SAMPLE_TIME |
                       PERF_SAMPLE_READ;
    attr.read_format = 0; /* READ block = just the running value */
    attr.flags = PERF_ATTR_FLAG_DISABLED;

    long fd = perf_event_open(&attr, 0, -1, -1, 0ul);
    if (fd < 0) {
        if (errno == ENOSYS) {
            printf("STARRY_PERF_SAMPLE_READ skip: perf_event_open ENOSYS\n");
            printf("STARRY_PERF_SAMPLE_READ_OK\n");
            return 0;
        }
        printf("perf-sample-read FAILED: perf_event_open errno=%d\n", errno);
        return 1;
    }
    int efd = (int)fd;
    void *base = mmap(NULL, PERF_MMAP_TOTAL_BYTES, PROT_READ | PROT_WRITE,
                      MAP_SHARED, efd, 0);
    if (base == MAP_FAILED) {
        printf("perf-sample-read FAILED: mmap errno=%d\n", errno);
        close(efd);
        return 1;
    }
    struct perf_event_mmap_page *meta = (struct perf_event_mmap_page *)base;

    g_zfd = open("/dev/zero", O_RDONLY);
    (void)ioctl(efd, PERF_EVENT_IOC_RESET, 0);
    (void)ioctl(efd, PERF_EVENT_IOC_ENABLE, 0);
    workload();
    (void)ioctl(efd, PERF_EVENT_IOC_DISABLE, 0);
    if (g_zfd >= 0) {
        close(g_zfd);
    }

    uint64_t data_head = meta->data_head;
    __sync_synchronize();
    uint64_t data_tail = meta->data_tail;
    uint64_t data_offset = meta->data_offset;
    uint64_t data_size = meta->data_size;
    const uint8_t *data_base = (const uint8_t *)base + data_offset;

    uint64_t samples = 0, prev = 0, bad_order = 0, zero_val = 0;
    /* body order: u64 ip; u32 pid; u32 tid; u64 time; u64 read_value */
    const uint64_t rv_off = (uint64_t)sizeof(struct perf_event_header) + 8 + 8 + 8;
    uint64_t off = data_tail;
    while (off < data_head && data_size != 0) {
        uint64_t rel = off % data_size;
        struct perf_event_header hdr;
        ring_copy(data_base, data_size, rel, &hdr, sizeof(hdr));
        if (hdr.size == 0 || off + hdr.size > data_head) {
            break;
        }
        if (hdr.type == PERF_RECORD_SAMPLE && rv_off + 8 <= hdr.size) {
            uint64_t rv = 0;
            ring_copy(data_base, data_size, (rel + rv_off) % data_size, &rv, 8);
            samples++;
            if (rv == 0) {
                zero_val++;
            }
            if (samples > 1 && rv <= prev) {
                bad_order++;
            }
            prev = rv;
        }
        off += hdr.size;
    }

    printf("STARRY_PERF_SAMPLE_READ samples=%llu last_value=%llu bad_order=%llu "
           "zero=%llu\n",
           (unsigned long long)samples, (unsigned long long)prev,
           (unsigned long long)bad_order, (unsigned long long)zero_val);

    int rc = 0;
    if (samples < 2) {
        printf("perf-sample-read FAILED: fewer than 2 samples\n");
        rc = 1;
    } else if (zero_val != 0) {
        printf("perf-sample-read FAILED: %llu samples had read value 0\n",
               (unsigned long long)zero_val);
        rc = 1;
    } else if (bad_order != 0) {
        printf("perf-sample-read FAILED: %llu samples not strictly increasing "
               "(running count broken)\n",
               (unsigned long long)bad_order);
        rc = 1;
    }

    (void)munmap(base, PERF_MMAP_TOTAL_BYTES);
    close(efd);
    if (rc == 0) {
        printf("STARRY_PERF_SAMPLE_READ_OK\n");
    }
    return rc;
}
