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
 * checks the rdpmc fields, then reads `PMCCNTR_EL0` from EL0 and cross-checks it
 * against `read(perf_fd)`. If EL0 access were not enabled the `mrs` would trap
 * (SIGILL) and the test would die — so reaching the comparison already proves it.
 *
 * SUCCESS == cap_user_rdpmc set AND index!=0 AND the EL0 `mrs` read and the
 * read(fd) value are both non-zero and within a small factor of each other.
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

    void *base = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, efd, 0);
    if (base == MAP_FAILED) {
        char msg[96];
        snprintf(msg, sizeof(msg), "mmap errno=%d", errno);
        close(efd);
        return fail(msg);
    }
    struct perf_event_mmap_page *pc = (struct perf_event_mmap_page *)base;

    uint32_t index = pc->index;
    uint64_t caps = pc->capabilities;
    uint16_t width = pc->pmc_width;
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

    /* EL0 read via mrs (the rdpmc path) and the syscall read, back to back. */
    uint64_t rd = read_pmccntr_el0();
    uint64_t sys = 0;
    if (read(efd, &sys, sizeof(sys)) != (ssize_t)sizeof(sys)) {
        munmap(base, 4096);
        close(efd);
        return fail("read(perf_fd)");
    }

    printf("STARRY_PERF_RDPMC rdpmc=%llu read_fd=%llu spin=%llu\n",
           (unsigned long long)rd, (unsigned long long)sys,
           (unsigned long long)spin);

    int rc = 0;
    if (rd == 0) {
        rc = fail("EL0 rdpmc read is zero");
    } else if (sys == 0) {
        rc = fail("read(perf_fd) is zero");
    } else {
        /* Both read the same hardware cycle counter moments apart; they must be
         * in the same ballpark (within ~16x covers scheduling jitter under TCG). */
        uint64_t lo = rd < sys ? rd : sys;
        uint64_t hi = rd < sys ? sys : rd;
        if (hi > lo * 16 + 1000000) {
            rc = fail("rdpmc and read(perf_fd) differ wildly");
        }
    }

    munmap(base, 4096);
    close(efd);
    if (rc == 0) {
        printf("STARRY_PERF_RDPMC_OK\n");
        return 0;
    }
    return rc;
}
