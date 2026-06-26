/*
 * perf_hw_fork_exit.c -- perf task-lifetime side-band records (FORK + EXIT).
 *
 * With attr.task set, the kernel emits PERF_RECORD_FORK when the monitored task
 * clones a child and PERF_RECORD_EXIT when the monitored task exits, into the
 * same mmap ring as the samples. `perf record` uses these to build the process
 * tree (follow forked children, mark tasks dead). Both records share one body:
 * subject pid/tid, parent ppid/ptid, a u64 time, then the sample_id trailer.
 *
 * This test drives exactly that: fork a child, open a per-task sampling event on
 * it with attr.task = attr.sample_id_all = 1 and enable_on_exec, mmap the ring,
 * then release the child. The child execs itself in --busy mode where it forks a
 * grandchild (-> the monitored child emits a FORK record), waits for it, spins
 * briefly, and exits (-> the monitored child emits an EXIT record). We then walk
 * the ring and confirm both a FORK and an EXIT record are present.
 *
 * attr.comm / attr.mmap2 are deliberately NOT set, so the only side-band records
 * are FORK and EXIT (plus any PERF_RECORD_SAMPLE), keeping the assertions tight.
 *
 * SUCCESS == at least one PERF_RECORD_FORK AND at least one PERF_RECORD_EXIT.
 * Prints the single sentinel STARRY_PERF_FORK_EXIT_OK.
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
#include <sys/wait.h>
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
/* This test validates side-band records (FORK/EXIT), not samples. Set the period
 * far above the busy loop's cycle budget so the counter effectively never
 * overflows: the data ring then holds only the side-band records and never wraps.
 * (A small period let the grouped run — where the loop runs longer than in
 * isolation — produce ~1000 samples that wrapped the 8-page ring and overwrote
 * the EXIT record, which is written last, at the monitored task's exit.) */
#define SAMPLE_PERIOD 0x40000000ull

#ifndef PERF_EVENT_IOC_DISABLE
#define PERF_EVENT_IOC_DISABLE 0x2401u
#endif
/* perf_event_attr flag bit positions (see the bitfield in <linux/perf_event.h>). */
#define PERF_ATTR_FLAG_DISABLED (1ull << 0)
#define PERF_ATTR_FLAG_ENABLE_ON_EXEC (1ull << 12)
#define PERF_ATTR_FLAG_TASK (1ull << 13)
#define PERF_ATTR_FLAG_SAMPLE_ID_ALL (1ull << 18)

#define PERF_RECORD_EXIT 4u
#define PERF_RECORD_FORK 7u
#define PERF_RECORD_SAMPLE 9u

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
    printf("perf-fork-exit FAILED: %s\n", reason);
    return 1;
}

int main(int argc, char **argv) {
#if !defined(__aarch64__)
    /* Hardware-PMU perf is aarch64-only (ARM PMUv3); skip-as-pass on other
     * architectures so the cross-arch grouped C build/run stays green. */
    printf("STARRY_PERF_FORK_EXIT_OK\n");
    return 0;
#endif
    if (argc > 1 && strcmp(argv[1], "--busy") == 0) {
        /* Monitored task: fork a grandchild (-> FORK record), reap it, spin a
         * little, then return (-> EXIT record). */
        pid_t gc = fork();
        if (gc == 0) {
            volatile uint64_t s = 0;
            for (uint64_t i = 0; i < 20000000ull; i++) {
                s += i;
            }
            _exit((int)(s & 1));
        }
        if (gc > 0) {
            waitpid(gc, NULL, 0);
        }
        volatile uint64_t spin = 0;
        for (uint64_t i = 0; i < 100000000ull; i++) {
            spin += i;
        }
        return (int)(spin & 1);
    }

    int go[2];
    if (pipe(go) != 0) {
        return fail("pipe");
    }
    pid_t child = fork();
    if (child < 0) {
        return fail("fork");
    }
    if (child == 0) {
        close(go[1]);
        char b;
        (void)!read(go[0], &b, 1);
        close(go[0]);
        char *av[] = {argv[0], (char *)"--busy", NULL};
        execv(argv[0], av);
        _exit(127);
    }
    close(go[0]);

    struct perf_event_attr attr;
    memset(&attr, 0, sizeof(attr));
    attr.type = PERF_TYPE_RAW;
    attr.config = ARM_PMU_EVT_CPU_CYCLES;
    attr.size = (uint32_t)sizeof(attr);
    attr.sample_period = SAMPLE_PERIOD;
    attr.sample_type = PERF_SAMPLE_IP | PERF_SAMPLE_TID | PERF_SAMPLE_TIME;
    attr.flags = PERF_ATTR_FLAG_DISABLED | PERF_ATTR_FLAG_ENABLE_ON_EXEC |
                 PERF_ATTR_FLAG_TASK | PERF_ATTR_FLAG_SAMPLE_ID_ALL;

    long fd = perf_event_open(&attr, child, -1, -1, 0ul);
    if (fd < 0) {
        (void)!write(go[1], "g", 1);
        close(go[1]);
        waitpid(child, NULL, 0);
        return fail("perf_event_open");
    }
    int efd = (int)fd;

    void *base = mmap(NULL, PERF_MMAP_TOTAL_BYTES, PROT_READ | PROT_WRITE,
                      MAP_SHARED, efd, 0);
    if (base == MAP_FAILED) {
        (void)!write(go[1], "g", 1);
        close(go[1]);
        waitpid(child, NULL, 0);
        close(efd);
        return fail("mmap ring");
    }
    struct perf_event_mmap_page *meta = (struct perf_event_mmap_page *)base;

    /* Release the child: it execs (-> enable_on_exec), forks a grandchild
     * (-> FORK), runs, and exits (-> EXIT). */
    (void)!write(go[1], "g", 1);
    close(go[1]);
    int status = 0;
    waitpid(child, &status, 0);
    (void)ioctl(efd, PERF_EVENT_IOC_DISABLE, 0);

    uint64_t data_head = meta->data_head;
    __sync_synchronize();
    uint64_t data_tail = meta->data_tail;
    uint64_t data_offset = meta->data_offset;
    uint64_t data_size = meta->data_size;
    const uint8_t *data_base = (const uint8_t *)base + data_offset;

    uint64_t n_fork = 0, n_exit = 0, n_sample = 0;
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
        if (hdr.type == PERF_RECORD_FORK) {
            n_fork++;
        } else if (hdr.type == PERF_RECORD_EXIT) {
            n_exit++;
        } else if (hdr.type == PERF_RECORD_SAMPLE) {
            n_sample++;
        }
        off += hdr.size;
    }

    printf("STARRY_PERF_FORK_EXIT fork=%llu exit=%llu samples=%llu\n",
           (unsigned long long)n_fork, (unsigned long long)n_exit,
           (unsigned long long)n_sample);

    int rc = 0;
    if (n_fork == 0) {
        rc = fail("no PERF_RECORD_FORK record");
    } else if (n_exit == 0) {
        rc = fail("no PERF_RECORD_EXIT record");
    }

    (void)munmap(base, PERF_MMAP_TOTAL_BYTES);
    close(efd);
    if (rc == 0) {
        printf("STARRY_PERF_FORK_EXIT_OK\n");
        return 0;
    }
    return rc;
}
