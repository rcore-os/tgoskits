/*
 * perf_hw_rdpmc.c -- userspace `rdpmc` (EL0 counter read) ABI test.
 *
 * A self-monitoring process can read its hardware counter without a syscall by
 * mapping the event's `perf_event_mmap_page` and reading the counter directly
 * with `mrs` from EL0. This requires the kernel to (1) enable EL0 PMU read
 * access (`PMUSERENR_EL0`) and (2) fill the mmap page's rdpmc metadata
 * (`cap_user_rdpmc`, the 1-based `index`, `pmc_width`).
 *
 * This test opens a disabled self counting CPU_CYCLES event. Per-task events use
 * a scheduler-owned programmable counter, so userspace selects the counter from
 * the mmap page's 1-based index instead of assuming the system-wide cycle
 * counter. After enable + sched_yield makes the scheduler publish the event, the
 * test reads that counter from EL0 and cross-checks it against read(perf_fd).
 * If EL0 access were not enabled the system-register access would trap (SIGILL),
 * so reaching the comparison already proves it.
 *
 * SUCCESS == cap_user_rdpmc set AND the active mmap count is non-zero and does
 * not exceed a following read(fd), then after disable + sched-out the mmap page
 * reports index=0 and retains exactly the read(fd) total in offset.
 * Prints the single sentinel STARRY_PERF_RDPMC_OK.
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <sched.h>
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
#define PERF_ATTR_DISABLED (1ull << 0)

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

/* Read the 1-based counter selected by perf_event_mmap_page.index. Event
 * counters are selected through PMSELR_EL0 and read through PMXEVCNTR_EL0;
 * index 32 names the dedicated cycle counter. PMUSERENR_EL0.ER/CR gate these
 * accesses, so a successful read also proves that the kernel enabled EL0. */
static inline uint64_t read_pmu_counter(uint32_t index) {
#if defined(__aarch64__)
    uint64_t v;
    if (index == CYCLE_PAGE_INDEX) {
        __asm__ volatile("mrs %0, pmccntr_el0" : "=r"(v));
        return v;
    }
    uint64_t selector = (uint64_t)(index - 1u);
    __asm__ volatile("msr pmselr_el0, %1\n\t"
                     "isb\n\t"
                     "mrs %0, pmxevcntr_el0"
                     : "=r"(v)
                     : "r"(selector)
                     : "memory");
    return v & UINT32_MAX;
#else
    (void)index;
    return 0; /* unreachable: main() skips on non-aarch64 */
#endif
}

static uint64_t read_mmap_count(struct perf_event_mmap_page *pc,
                                uint32_t *index_out, int64_t *offset_out) {
    uint32_t seq;
    uint32_t index;
    uint16_t width;
    int64_t offset;
    uint64_t count;

    for (;;) {
        seq = __atomic_load_n(&pc->lock, __ATOMIC_ACQUIRE);
        if (seq & 1u) {
            continue;
        }
        index = __atomic_load_n(&pc->index, __ATOMIC_RELAXED);
        offset = __atomic_load_n(&pc->offset, __ATOMIC_RELAXED);
        width = __atomic_load_n(&pc->pmc_width, __ATOMIC_RELAXED);
        count = (uint64_t)offset;
        if (index != 0) {
            uint64_t pmc = read_pmu_counter(index);
            if (width < 64) {
                pmc <<= 64 - width;
                pmc = (uint64_t)((int64_t)pmc >> (64 - width));
            }
            count += pmc;
        }
        if (__atomic_load_n(&pc->lock, __ATOMIC_ACQUIRE) == seq) {
            break;
        }
    }

    *index_out = index;
    *offset_out = offset;
    return count;
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
    attr.flags = PERF_ATTR_DISABLED;
    /* counting event: no sample_period/freq. */

    /* Self-monitoring: pid=0 (this process), cpu=-1 (any cpu). */
    long fd = perf_event_open(&attr, 0, -1, -1, 0ul);
    if (fd < 0) {
        char msg[96];
        snprintf(msg, sizeof(msg), "perf_event_open errno=%d", errno);
        return fail(msg);
    }
    int efd = (int)fd;

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
    if (index != 0) {
        munmap(base, 4096);
        close(efd);
        return fail("disabled event exposes a hardware counter before ENABLE");
    }
    if (width != 32) {
        munmap(base, 4096);
        close(efd);
        return fail("per-task programmable counter width is not 32 bits");
    }

    if (ioctl(efd, PERF_EVENT_IOC_ENABLE, 0) != 0) {
        munmap(base, 4096);
        close(efd);
        return fail("ioctl(ENABLE)");
    }
    /* ENABLE records intent; the per-task PMU slot is programmed at sched-in. */
    if (sched_yield() != 0) {
        munmap(base, 4096);
        close(efd);
        return fail("sched_yield after ENABLE");
    }

    /* Burn some cycles so the counter advances measurably. */
    volatile uint64_t spin = 0;
    for (uint64_t i = 0; i < 20000000ull; i++) {
        spin += i;
    }
    (void)spin;

    /* Sleeping necessarily takes this event through sched-out and sched-in.
     * The completed slice must move into offset before a new live hardware
     * counter is exposed. sched_yield() alone is not sufficient when no peer
     * task is runnable. */
    if (usleep(10000) != 0) {
        munmap(base, 4096);
        close(efd);
        return fail("usleep before active mmap-page read");
    }

    /* Read the live total through Linux's mmap-page contract: offset plus the
     * selected hardware counter, guarded by the metadata seqlock. */
    int64_t offset = 0;
    uint64_t rd = read_mmap_count(pc, &index, &offset);
    uint64_t sys = 0;
    if (read(efd, &sys, sizeof(sys)) != (ssize_t)sizeof(sys)) {
        munmap(base, 4096);
        close(efd);
        return fail("read(perf_fd)");
    }

    printf("STARRY_PERF_RDPMC active_index=%u active_offset=%lld "
           "rdpmc=%llu read_fd=%llu spin=%llu\n",
           index, (long long)offset, (unsigned long long)rd,
           (unsigned long long)sys, (unsigned long long)spin);

    int rc = 0;
    if (index == 0) {
        rc = fail("enabled event did not publish a hardware counter");
    } else if (index >= CYCLE_PAGE_INDEX) {
        rc = fail("per-task event published an invalid programmable counter");
    } else if (offset <= 0) {
        rc = fail("sched-out count not preserved in active mmap-page offset");
    } else if (rd == 0) {
        rc = fail("EL0 rdpmc read is zero");
    } else if (sys == 0) {
        rc = fail("read(perf_fd) is zero");
    } else {
        /* read(fd) runs after the direct read, so it must include at least the
         * total published through the mmap page. */
        if (sys < rd) {
            rc = fail("read(perf_fd) went backwards from mmap-page count");
        }
    }

    if (rc == 0 && ioctl(efd, PERF_EVENT_IOC_DISABLE, 0) != 0) {
        rc = fail("ioctl(DISABLE)");
    }
    /* Force the disabled task through sched-out. The inactive metadata must
     * hide the hardware slot and retain the completed total in offset. */
    if (rc == 0 && sched_yield() != 0) {
        rc = fail("sched_yield after DISABLE");
    }
    if (rc == 0) {
        uint32_t inactive_index = UINT32_MAX;
        int64_t inactive_offset = -1;
        uint64_t mmap_total =
            read_mmap_count(pc, &inactive_index, &inactive_offset);
        uint64_t fd_total = 0;
        if (read(efd, &fd_total, sizeof(fd_total)) !=
            (ssize_t)sizeof(fd_total)) {
            rc = fail("read(perf_fd) after DISABLE");
        } else {
            printf("STARRY_PERF_RDPMC inactive_index=%u inactive_offset=%lld "
                   "mmap_total=%llu read_fd=%llu\n",
                   inactive_index, (long long)inactive_offset,
                   (unsigned long long)mmap_total,
                   (unsigned long long)fd_total);
            if (inactive_index != 0) {
                rc = fail("inactive mmap page still exposes a hardware counter");
            } else if (inactive_offset <= 0) {
                rc = fail("inactive mmap page did not retain the completed count");
            } else if (mmap_total != fd_total) {
                rc = fail("inactive mmap-page total differs from read(perf_fd)");
            }
        }
    }

    if (rc == 0 && ioctl(efd, PERF_EVENT_IOC_RESET, 0) != 0) {
        rc = fail("ioctl(RESET)");
    }
    if (rc == 0) {
        uint32_t reset_index = UINT32_MAX;
        int64_t reset_offset = -1;
        uint64_t mmap_total = read_mmap_count(pc, &reset_index, &reset_offset);
        uint64_t fd_total = UINT64_MAX;
        if (read(efd, &fd_total, sizeof(fd_total)) !=
            (ssize_t)sizeof(fd_total)) {
            rc = fail("read(perf_fd) after RESET");
        } else if (reset_index != 0 || reset_offset != 0 || mmap_total != 0 ||
                   fd_total != 0) {
            rc = fail("disabled RESET did not zero mmap-page and fd counts");
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
