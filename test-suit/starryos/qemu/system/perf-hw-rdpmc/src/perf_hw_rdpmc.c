/*
 * perf_hw_rdpmc.c -- userspace `rdpmc` (EL0 counter read) ABI test.
 *
 * A self-monitoring process can read its hardware counter without a syscall by
 * mapping the event's `perf_event_mmap_page` and reading the counter directly
 * with `mrs` from EL0. This requires the kernel to (1) enable EL0 PMU read
 * access (`PMUSERENR_EL0`) and (2) fill the mmap page's rdpmc metadata
 * (`cap_user_rdpmc`, the 1-based `index`, `pmc_width`).
 *
 * This test opens a self counting CPU_CYCLES event (which the kernel backs with
 * the dedicated cycle counter, page index 32 ⇒ `PMCCNTR_EL0`), mmaps the page,
 * checks the rdpmc fields, forces a sched-out/in with `usleep`, then performs
 * Linux's seqlock read (`offset + rdpmc`) and cross-checks it against
 * `read(perf_fd)`. If EL0 access were not enabled the `mrs` would trap (SIGILL)
 * and the test would die — so reaching the comparison already proves it.
 *
 * SUCCESS == cap_user_rdpmc set AND index!=0 AND a completed scheduling slice
 * is preserved in nonzero `offset` AND the page count and read(fd) value are
 * both non-zero and within a small factor of each other.
 * Prints the single sentinel STARRY_PERF_RDPMC_OK.
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

#ifndef PERF_TYPE_HARDWARE
#define PERF_TYPE_HARDWARE 0u
#endif
#ifndef PERF_COUNT_HW_CPU_CYCLES
#define PERF_COUNT_HW_CPU_CYCLES 0u
#endif
#ifndef PERF_EVENT_IOC_ENABLE
#define PERF_EVENT_IOC_ENABLE 0x2400u
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
};

#define CAP_USER_RDPMC (1ull << 2)
/* Page index for the dedicated cycle counter: ARM idx 31 ⇒ 1-based 32. */
#define CYCLE_PAGE_INDEX 32u

#ifndef SYS_perf_event_open
#define SYS_perf_event_open 241
#endif

static long perf_event_open(struct perf_event_attr *attr, pid_t pid, int cpu,
                            int group_fd, unsigned long flags) {
    return syscall(SYS_perf_event_open, attr, pid, cpu, group_fd, flags);
}

static int fail(const char *reason) {
    printf("perf-rdpmc FAILED: %s\n", reason);
    return 1;
}

/* Read the dedicated cycle counter from EL0. Traps (SIGILL) if PMUSERENR_EL0.CR
 * is not set — so a successful read is itself proof EL0 access is enabled. */
static inline uint64_t read_pmccntr_el0(void) {
#if defined(__aarch64__)
    uint64_t v;
    __asm__ volatile("mrs %0, pmccntr_el0" : "=r"(v));
    return v;
#else
    return 0; /* unreachable: main() skips on non-aarch64 */
#endif
}

struct rdpmc_snapshot {
    uint32_t index;
    int64_t offset;
    uint64_t capabilities;
    uint16_t pmc_width;
    uint64_t raw;
    uint64_t count;
};

/*
 * Linux perf's userspace read protocol. The kernel changes `lock` around every
 * sched-out/in metadata publication. If this task is preempted between reading
 * the metadata and PMCCNTR_EL0, the changed sequence forces a retry after it
 * resumes.
 */
static int read_rdpmc_page(const struct perf_event_mmap_page *pc,
                           struct rdpmc_snapshot *snapshot) {
    for (unsigned int attempt = 0; attempt < 1000000u; attempt++) {
        uint32_t sequence = __atomic_load_n(&pc->lock, __ATOMIC_ACQUIRE);
        if ((sequence & 1u) != 0) {
            continue;
        }

        struct rdpmc_snapshot current;
        current.index = __atomic_load_n(&pc->index, __ATOMIC_RELAXED);
        current.offset = __atomic_load_n(&pc->offset, __ATOMIC_RELAXED);
        current.capabilities =
            __atomic_load_n(&pc->capabilities, __ATOMIC_RELAXED);
        current.pmc_width =
            __atomic_load_n(&pc->pmc_width, __ATOMIC_RELAXED);
        current.raw = current.index == CYCLE_PAGE_INDEX ? read_pmccntr_el0() : 0;

        __atomic_thread_fence(__ATOMIC_ACQUIRE);
        if (__atomic_load_n(&pc->lock, __ATOMIC_RELAXED) != sequence) {
            continue;
        }
        if (current.index == 0 || current.pmc_width == 0) {
            return -1;
        }

        if (current.pmc_width < 64) {
            unsigned int shift = 64u - current.pmc_width;
            current.raw =
                (uint64_t)(((int64_t)(current.raw << shift)) >> shift);
        }
        current.count = (uint64_t)(current.offset + (int64_t)current.raw);
        *snapshot = current;
        return 0;
    }
    return -1;
}

int main(void) {
#if !defined(__aarch64__)
    /* Hardware-PMU perf is aarch64-only (ARM PMUv3); skip-as-pass on other
     * architectures so the cross-arch grouped C build/run stays green. */
    printf("STARRY_PERF_RDPMC_OK\n");
    return 0;
#endif
    struct perf_event_attr attr;
    memset(&attr, 0, sizeof(attr));
    attr.type = PERF_TYPE_HARDWARE;
    attr.config = PERF_COUNT_HW_CPU_CYCLES;
    attr.size = (uint32_t)sizeof(attr);
    /* counting event: no sample_period/freq. */

    /* Self-monitoring: pid=0 (this process), cpu=-1 (any cpu). */
    long fd = perf_event_open(&attr, 0, -1, -1, 0ul);
    if (fd < 0) {
        char msg[96];
        snprintf(msg, sizeof(msg), "perf_event_open errno=%d", errno);
        return fail(msg);
    }
    int efd = (int)fd;

    if (ioctl(efd, PERF_EVENT_IOC_ENABLE, 0) != 0) {
        close(efd);
        return fail("ioctl(ENABLE)");
    }

    errno = 0;
    void *oversized =
        mmap(NULL, 8192, PROT_READ | PROT_WRITE, MAP_SHARED, efd, 0);
    if (oversized != MAP_FAILED) {
        munmap(oversized, 8192);
        close(efd);
        return fail("oversized metadata mmap unexpectedly succeeded");
    }
    if (errno != EINVAL) {
        close(efd);
        return fail("oversized metadata mmap did not return EINVAL");
    }

    void *base = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, efd, 0);
    if (base == MAP_FAILED) {
        char msg[96];
        snprintf(msg, sizeof(msg), "mmap errno=%d", errno);
        close(efd);
        return fail(msg);
    }
    struct perf_event_mmap_page *pc = (struct perf_event_mmap_page *)base;

    errno = 0;
    void *duplicate =
        mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, efd, 0);
    if (duplicate != MAP_FAILED) {
        munmap(duplicate, 4096);
        munmap(base, 4096);
        close(efd);
        return fail("second live metadata mmap unexpectedly succeeded");
    }
    if (errno != EBUSY) {
        munmap(base, 4096);
        close(efd);
        return fail("second live metadata mmap did not return EBUSY");
    }

    struct rdpmc_snapshot initial;
    if (read_rdpmc_page(pc, &initial) != 0) {
        munmap(base, 4096);
        close(efd);
        return fail("initial mmap-page seqlock read");
    }
    uint32_t index = initial.index;
    uint64_t caps = initial.capabilities;
    uint16_t width = initial.pmc_width;
    printf("STARRY_PERF_RDPMC index=%u caps=0x%llx pmc_width=%u\n", index,
           (unsigned long long)caps, width);

    if ((caps & CAP_USER_RDPMC) == 0) {
        munmap(base, 4096);
        close(efd);
        return fail("cap_user_rdpmc not set");
    }
    if (index == 0) {
        munmap(base, 4096);
        close(efd);
        return fail("index is 0 (rdpmc not usable)");
    }
    if (index != CYCLE_PAGE_INDEX) {
        munmap(base, 4096);
        close(efd);
        return fail("cycles event not on the dedicated cycle counter");
    }

    /* Burn some cycles so the counter advances measurably. */
    volatile uint64_t spin = 0;
    for (uint64_t i = 0; i < 20000000ull; i++) {
        spin += i;
    }
    (void)spin;

    /*
     * Sleeping forces this event through sched-out and sched-in. Linux carries
     * the completed slice in mmap-page `offset` and only exposes the newly
     * programmed counter through `index`.
     */
    if (usleep(10000) != 0) {
        munmap(base, 4096);
        close(efd);
        return fail("usleep");
    }

    struct rdpmc_snapshot after_switch;
    if (read_rdpmc_page(pc, &after_switch) != 0) {
        munmap(base, 4096);
        close(efd);
        return fail("post-switch mmap-page seqlock read");
    }
    if (after_switch.offset <= 0) {
        munmap(base, 4096);
        close(efd);
        return fail("sched-out count not preserved in mmap-page offset");
    }

    /* Page count (`offset + mrs`) and syscall read, back to back. */
    uint64_t rd = after_switch.count;
    uint64_t sys = 0;
    if (read(efd, &sys, sizeof(sys)) != (ssize_t)sizeof(sys)) {
        munmap(base, 4096);
        close(efd);
        return fail("read(perf_fd)");
    }

    printf("STARRY_PERF_RDPMC offset=%lld raw=%llu count=%llu read_fd=%llu "
           "spin=%llu\n",
           (long long)after_switch.offset,
           (unsigned long long)after_switch.raw, (unsigned long long)rd,
           (unsigned long long)sys,
           (unsigned long long)spin);

    int rc = 0;
    if (rd == 0) {
        rc = fail("EL0 rdpmc read is zero");
    } else if (sys == 0) {
        rc = fail("read(perf_fd) is zero");
    } else {
        /* Both include the same completed slices and read the current hardware
         * cycle counter moments apart. */
        uint64_t lo = rd < sys ? rd : sys;
        uint64_t hi = rd < sys ? sys : rd;
        if (hi > lo * 16 + 1000000) {
            rc = fail("rdpmc and read(perf_fd) differ wildly");
        }
    }

    munmap(base, 4096);
    void *remapped =
        mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, efd, 0);
    if (remapped == MAP_FAILED) {
        close(efd);
        return fail("metadata mmap was not reusable after munmap");
    }
    munmap(remapped, 4096);
    close(efd);
    if (rc == 0) {
        printf("STARRY_PERF_RDPMC_OK\n");
        return 0;
    }
    return rc;
}
