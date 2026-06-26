/*
 * perf_hw_ring_merge.c -- multi-event ring sharing (`perf record -e a,b`) test.
 *
 * `perf record` with more than one event opens each event separately, mmaps only
 * the leader, and points the others at the leader's ring with
 * `PERF_EVENT_IOC_SET_OUTPUT`. The samples of all events then interleave in one
 * buffer and are told apart by the `PERF_SAMPLE_ID` field (each event's unique
 * `PERF_EVENT_IOC_ID`).
 *
 * This test opens two system-wide sampling events (both CPU_CYCLES, so both count
 * under QEMU TCG), mmaps the first (A), redirects the second (B) into A's ring
 * with SET_OUTPUT, reads each event's id with IOC_ID, runs a busy loop, then
 * walks A's ring and confirms it contains samples tagged with BOTH ids — proving
 * B's overflow samples really landed in A's ring and stayed distinguishable.
 *
 * SUCCESS == distinct non-zero ids AND A's ring holds >=1 sample with id==id_A
 * AND >=1 with id==id_B. Prints the single sentinel STARRY_PERF_RING_MERGE_OK.
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
#define PERF_SAMPLE_ID (1ull << 6)
#define SAMPLE_PERIOD 50000ull

#ifndef PERF_EVENT_IOC_ENABLE
#define PERF_EVENT_IOC_ENABLE 0x2400u
#endif
#ifndef PERF_EVENT_IOC_DISABLE
#define PERF_EVENT_IOC_DISABLE 0x2401u
#endif
#ifndef PERF_EVENT_IOC_ID
#define PERF_EVENT_IOC_ID _IOR('$', 7, uint64_t *)
#endif
#ifndef PERF_EVENT_IOC_SET_OUTPUT
#define PERF_EVENT_IOC_SET_OUTPUT _IO('$', 5)
#endif
#define PERF_ATTR_FLAG_DISABLED (1ull << 0)
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

static void init_attr(struct perf_event_attr *attr) {
    memset(attr, 0, sizeof(*attr));
    attr->type = PERF_TYPE_RAW;
    attr->config = ARM_PMU_EVT_CPU_CYCLES;
    attr->size = (uint32_t)sizeof(*attr);
    attr->sample_period = SAMPLE_PERIOD;
    attr->sample_type = PERF_SAMPLE_IP | PERF_SAMPLE_ID;
    attr->flags = PERF_ATTR_FLAG_DISABLED;
}

static int fail(const char *reason) {
    printf("perf-ring-merge FAILED: %s\n", reason);
    return 1;
}

int main(void) {
    struct perf_event_attr attr;

    /* Leader A: system-wide on cpu0, mmap'd. */
    init_attr(&attr);
    long la = perf_event_open(&attr, -1, 0, -1, 0ul);
    if (la < 0) {
        return fail("perf_event_open(A)");
    }
    int afd = (int)la;

    void *base = mmap(NULL, PERF_MMAP_TOTAL_BYTES, PROT_READ | PROT_WRITE,
                      MAP_SHARED, afd, 0);
    if (base == MAP_FAILED) {
        close(afd);
        return fail("mmap ring");
    }
    struct perf_event_mmap_page *meta = (struct perf_event_mmap_page *)base;

    /* Second event B: system-wide on cpu0, NOT mmap'd — redirected into A. */
    init_attr(&attr);
    long lb = perf_event_open(&attr, -1, 0, -1, 0ul);
    if (lb < 0) {
        munmap(base, PERF_MMAP_TOTAL_BYTES);
        close(afd);
        return fail("perf_event_open(B)");
    }
    int bfd = (int)lb;

    uint64_t id_a = 0, id_b = 0;
    if (ioctl(afd, PERF_EVENT_IOC_ID, &id_a) != 0 || id_a == 0) {
        return fail("PERF_EVENT_IOC_ID(A)");
    }
    if (ioctl(bfd, PERF_EVENT_IOC_ID, &id_b) != 0 || id_b == 0) {
        return fail("PERF_EVENT_IOC_ID(B)");
    }
    if (id_a == id_b) {
        return fail("event ids not distinct");
    }
    if (ioctl(bfd, PERF_EVENT_IOC_SET_OUTPUT, afd) != 0) {
        char msg[96];
        snprintf(msg, sizeof(msg), "PERF_EVENT_IOC_SET_OUTPUT errno=%d", errno);
        return fail(msg);
    }

    if (ioctl(afd, PERF_EVENT_IOC_ENABLE, 0) != 0 ||
        ioctl(bfd, PERF_EVENT_IOC_ENABLE, 0) != 0) {
        return fail("ioctl(ENABLE)");
    }
    volatile uint64_t spin = 0;
    for (uint64_t i = 0; i < 60000000ull; i++) {
        spin += i;
    }
    (void)ioctl(afd, PERF_EVENT_IOC_DISABLE, 0);
    (void)ioctl(bfd, PERF_EVENT_IOC_DISABLE, 0);

    uint64_t data_head = meta->data_head;
    __sync_synchronize();
    uint64_t data_tail = meta->data_tail;
    uint64_t data_offset = meta->data_offset;
    uint64_t data_size = meta->data_size;
    const uint8_t *data_base = (const uint8_t *)base + data_offset;

    uint64_t n_a = 0, n_b = 0, n_other = 0;
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
            /* body: IP (8) then ID (8). */
            uint64_t id = 0;
            uint64_t ibody = rel + sizeof(hdr) + 8;
            for (size_t b = 0; b < sizeof(id); b++) {
                ((uint8_t *)&id)[b] = data_base[(ibody + b) % data_size];
            }
            if (id == id_a) {
                n_a++;
            } else if (id == id_b) {
                n_b++;
            } else {
                n_other++;
            }
        }
        off += hdr.size;
    }

    printf("STARRY_PERF_RING_MERGE id_a=%llu id_b=%llu n_a=%llu n_b=%llu "
           "n_other=%llu\n",
           (unsigned long long)id_a, (unsigned long long)id_b,
           (unsigned long long)n_a, (unsigned long long)n_b,
           (unsigned long long)n_other);

    int rc = 0;
    if (n_a == 0) {
        rc = fail("no leader (A) samples in the ring");
    } else if (n_b == 0) {
        rc = fail("no redirected (B) samples in the leader's ring");
    }

    munmap(base, PERF_MMAP_TOTAL_BYTES);
    close(bfd);
    close(afd);
    if (rc == 0) {
        printf("STARRY_PERF_RING_MERGE_OK\n");
        return 0;
    }
    return rc;
}
