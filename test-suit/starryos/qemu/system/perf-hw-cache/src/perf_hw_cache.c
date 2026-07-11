/*
 * perf_hw_cache.c -- PERF_TYPE_HW_CACHE combinatorial cache events.
 *
 * `perf stat -e L1-dcache-load-misses,LLC-loads,...` opens PERF_TYPE_HW_CACHE
 * events whose config packs cache_id | (op<<8) | (result<<16). These used to be
 * rejected at open (only PERF_TYPE_HARDWARE cache-references/misses worked). This
 * test opens the common, ARM-defined combinations and asserts each opens and is
 * readable, and that an unsupported combination (an L1D PREFETCH, which ARM
 * PMUv3 has no event for) is correctly rejected.
 *
 * CI note: QEMU-TCG's PMU implements no cache events, so on QEMU every mapped
 * event is rejected at open with ENOSYS/EOPNOTSUPP by the `event_supported` gate
 * -- that is a QEMU limitation, not a mapping error. This test therefore accepts,
 * for each well-defined combination, EITHER a successful open+read (real hardware
 * where the event exists) OR a clean unsupported-event rejection (QEMU). Count
 * accuracy is validated on the board. What it hard-asserts everywhere:
 *   - a mapped combination never fails with a routing error (e.g. EINVAL) or a
 *     crash: it is either accepted or cleanly reported unsupported;
 *   - an UNSUPPORTED combination (an L1D PREFETCH -- ARM PMUv3 has no such event)
 *     is rejected at open.
 * On success exactly one line `STARRY_PERF_HW_CACHE_OK` is printed.
 *
 * aarch64-only (ARM PMUv3); skips-as-pass elsewhere.
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

#define PERF_TYPE_HW_CACHE 3u

/* config = cache_id | (op << 8) | (result << 16). */
#define C_L1D 0ull
#define C_L1I 1ull
#define C_LL 2ull
#define C_DTLB 3ull
#define C_ITLB 4ull
#define C_BPU 5ull
#define OP_READ 0ull
#define OP_WRITE 1ull
#define OP_PREFETCH 2ull
#define RES_ACCESS 0ull
#define RES_MISS 1ull
#define CACHE_CFG(id, op, res) ((id) | ((op) << 8) | ((res) << 16))

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

#ifndef SYS_perf_event_open
#define SYS_perf_event_open 241
#endif

static long perf_event_open(struct perf_event_attr *attr, pid_t pid, int cpu,
                            int group_fd, unsigned long flags) {
    return syscall(SYS_perf_event_open, attr, pid, cpu, group_fd, flags);
}

static int open_cache(uint64_t config) {
    struct perf_event_attr attr;
    memset(&attr, 0, sizeof(attr));
    attr.type = PERF_TYPE_HW_CACHE;
    attr.size = (uint32_t)sizeof(attr);
    attr.config = config;
    return (int)perf_event_open(&attr, 0, -1, -1, 0ul);
}

int main(void) {
#if !defined(__aarch64__)
    printf("STARRY_PERF_HW_CACHE_OK\n");
    return 0;
#endif
    struct cache_event {
        const char *name;
        uint64_t config;
    } supported[] = {
        {"L1-dcache-loads", CACHE_CFG(C_L1D, OP_READ, RES_ACCESS)},
        {"L1-dcache-load-misses", CACHE_CFG(C_L1D, OP_READ, RES_MISS)},
        {"L1-icache-loads", CACHE_CFG(C_L1I, OP_READ, RES_ACCESS)},
        {"LLC-loads", CACHE_CFG(C_LL, OP_READ, RES_ACCESS)},
        {"LLC-load-misses", CACHE_CFG(C_LL, OP_READ, RES_MISS)},
        {"dTLB-load-misses", CACHE_CFG(C_DTLB, OP_READ, RES_MISS)},
        {"iTLB-load-misses", CACHE_CFG(C_ITLB, OP_READ, RES_MISS)},
        {"branch-misses", CACHE_CFG(C_BPU, OP_READ, RES_MISS)},
    };
    const size_t ns = sizeof(supported) / sizeof(supported[0]);

    int rc = 0;
    unsigned opened = 0, unsupported = 0;
    for (size_t i = 0; i < ns; i++) {
        int fd = open_cache(supported[i].config);
        if (fd < 0) {
            /* Accept a clean unsupported-event rejection (QEMU-TCG has no cache
             * events); flag any other errno as a routing error. */
            if (errno == ENOSYS || errno == EOPNOTSUPP) {
                unsupported++;
                printf("STARRY_PERF_HW_CACHE %s unsupported-on-this-pmu errno=%d\n",
                       supported[i].name, errno);
            } else {
                printf("perf-hw-cache FAILED: %s open errno=%d (routing error, "
                       "expected open or unsupported)\n",
                       supported[i].name, errno);
                rc = 1;
            }
            continue;
        }
        uint64_t val = 0;
        ssize_t got = read(fd, &val, sizeof(val));
        if (got != (ssize_t)sizeof(val)) {
            printf("perf-hw-cache FAILED: %s not readable got=%zd errno=%d\n",
                   supported[i].name, got, errno);
            rc = 1;
        } else {
            opened++;
            printf("STARRY_PERF_HW_CACHE %s=%llu\n", supported[i].name,
                   (unsigned long long)val);
        }
        close(fd);
    }
    printf("STARRY_PERF_HW_CACHE summary opened=%u unsupported=%u\n", opened,
           unsupported);

    /* An L1D PREFETCH has no ARM PMUv3 event -> must be rejected. */
    int bad = open_cache(CACHE_CFG(C_L1D, OP_PREFETCH, RES_ACCESS));
    if (bad >= 0) {
        printf("perf-hw-cache FAILED: unsupported L1D-prefetch opened (should be "
               "rejected)\n");
        close(bad);
        rc = 1;
    } else {
        printf("STARRY_PERF_HW_CACHE L1D-prefetch correctly rejected errno=%d\n",
               errno);
    }

    if (rc == 0) {
        printf("STARRY_PERF_HW_CACHE_OK\n");
    }
    return rc;
}
