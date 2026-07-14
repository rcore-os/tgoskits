/*
 * perf-hw-smp-record-allcpu -- `perf record -a` per-CPU SAMPLING fan-out.
 *
 * One system-wide sampling event per cpu (pid=-1, cpu=i) must arm its
 * programmable counter AND its overflow IRQ ON core i (not the opening core), so
 * that each core's PMU overflow writes a PERF_RECORD_SAMPLE into THAT event's ring
 * stamped with cpu=i. Before the fan-out fix, a cpu-bound sampling event fell
 * through to the opening-core path: all N events armed on one core, so rings for
 * the other cores stayed empty (and every sample carried the opener's cpu).
 *
 * Flow: fork one child per cpu, each sched_setaffinity()-pinned to cpu i running a
 * read(/dev/zero) loop (syscall-heavy so the QEMU-TCG cycle counter overflows on
 * its core). The parent opens one sampling event per cpu (RAW CPU_CYCLES,
 * sample_type = IP|TID|TIME|CPU, disabled), mmaps each ring BEFORE enabling, then
 * enables all, waits for the children, and walks each ring parsing every
 * PERF_RECORD_SAMPLE's PERF_SAMPLE_CPU field.
 *
 * SUCCESS ==
 *     every perf_event_open(pid=-1, cpu=i) AND mmap succeeded
 *   AND every ring i received >= 1 PERF_RECORD_SAMPLE
 *   AND every sample in ring i carries cpu == i  (armed on the TARGET core).
 * On success exactly one line `STARRY_SMP_RECORD_ALLCPU_OK` is printed.
 *
 * aarch64-only (ARM PMUv3); skips-as-pass elsewhere.
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#define PERF_TYPE_RAW 4u
#define ARM_CPU_CYCLES 0x11ull

#define PERF_SAMPLE_IP (1ull << 0)
#define PERF_SAMPLE_TID (1ull << 1)
#define PERF_SAMPLE_TIME (1ull << 2)
#define PERF_SAMPLE_CPU (1ull << 7)

#define SAMPLE_PERIOD 100000ull

#define PERF_IOC_ENABLE 0x2400u
#define PERF_IOC_DISABLE 0x2401u
#define PERF_ATTR_DISABLED (1ull << 0)
#define PERF_RECORD_SAMPLE 9u

#ifndef SYS_perf_event_open
#define SYS_perf_event_open 241
#endif

#define NCPU 4
#define PERF_MMAP_PAGE_SIZE 4096u
#define PERF_MMAP_DATA_PAGES 8u
#define PERF_MMAP_TOTAL_BYTES                                                   \
    ((size_t)(1u + PERF_MMAP_DATA_PAGES) * PERF_MMAP_PAGE_SIZE)

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
_Static_assert(offsetof(struct perf_event_mmap_page, data_offset) == 1040, "do");

struct perf_event_header {
    uint32_t type;
    uint16_t misc;
    uint16_t size;
};

static long peo(struct perf_event_attr *a, pid_t pid, int cpu, int gfd,
                unsigned long fl) {
    return syscall(SYS_perf_event_open, a, pid, cpu, gfd, fl);
}

static void ring_copy(const uint8_t *base, uint64_t size, uint64_t at, void *dst,
                      size_t n) {
    for (size_t b = 0; b < n; b++) {
        ((uint8_t *)dst)[b] = base[(at + b) % size];
    }
}

/* Syscall-heavy loop so the target core's cycle counter overflows steadily. */
static void child_workload(void) {
    int zfd = open("/dev/zero", O_RDONLY);
    static uint8_t buf[4096];
    for (uint64_t i = 0; i < 400000ull; i++) {
        if (zfd >= 0) {
            if (read(zfd, buf, sizeof(buf)) < 0) {
                break;
            }
        }
    }
    if (zfd >= 0) {
        close(zfd);
    }
}

int main(void) {
#if !defined(__aarch64__)
    printf("STARRY_SMP_RECORD_ALLCPU_OK\n");
    return 0;
#endif
    pid_t pids[NCPU];

    /* One read()-heavy child pinned to each cpu. */
    for (int i = 0; i < NCPU; i++) {
        pid_t c = fork();
        if (c == 0) {
            cpu_set_t set;
            CPU_ZERO(&set);
            CPU_SET(i, &set);
            (void)sched_setaffinity(0, sizeof(set), &set);
            child_workload();
            _exit(0);
        }
        pids[i] = c;
        if (c < 0) {
            printf("smp-record-allcpu FAILED: fork[%d] errno=%d\n", i, errno);
            return 1;
        }
    }

    struct perf_event_attr attr;
    long fds[NCPU];
    void *bases[NCPU];
    int ok = 1;
    for (int i = 0; i < NCPU; i++) {
        fds[i] = -1;
        bases[i] = MAP_FAILED;
    }
    for (int i = 0; i < NCPU; i++) {
        for (size_t b = 0; b < sizeof(attr); b++) {
            ((volatile unsigned char *)&attr)[b] = 0;
        }
        attr.type = PERF_TYPE_RAW;
        attr.config = ARM_CPU_CYCLES;
        attr.size = sizeof(attr);
        attr.sample_period = SAMPLE_PERIOD;
        attr.sample_type =
            PERF_SAMPLE_IP | PERF_SAMPLE_TID | PERF_SAMPLE_TIME | PERF_SAMPLE_CPU;
        attr.flags = PERF_ATTR_DISABLED;
        /* pid=-1 (system-wide), cpu=i: sample all activity on core i. */
        fds[i] = peo(&attr, -1, i, -1, 0);
        if (fds[i] < 0) {
            printf("smp-record-allcpu FAILED: open(cpu=%d) errno=%d\n", i, errno);
            ok = 0;
            continue;
        }
        /* mmap the ring BEFORE ENABLE (enabling first registers a zero ring). */
        bases[i] = mmap(NULL, PERF_MMAP_TOTAL_BYTES, PROT_READ | PROT_WRITE,
                        MAP_SHARED, (int)fds[i], 0);
        if (bases[i] == MAP_FAILED) {
            printf("smp-record-allcpu FAILED: mmap(cpu=%d) errno=%d\n", i, errno);
            ok = 0;
        }
    }
    /* Enable all only after every ring is mapped. */
    for (int i = 0; i < NCPU; i++) {
        if (fds[i] >= 0 && bases[i] != MAP_FAILED) {
            (void)ioctl((int)fds[i], PERF_IOC_ENABLE, 0);
        }
    }

    for (int i = 0; i < NCPU; i++) {
        if (pids[i] > 0) {
            int st;
            waitpid(pids[i], &st, 0);
        }
    }

    for (int i = 0; i < NCPU; i++) {
        if (fds[i] < 0 || bases[i] == MAP_FAILED) {
            continue;
        }
        (void)ioctl((int)fds[i], PERF_IOC_DISABLE, 0);

        struct perf_event_mmap_page *meta =
            (struct perf_event_mmap_page *)bases[i];
        uint64_t data_head = meta->data_head;
        __sync_synchronize();
        uint64_t data_tail = meta->data_tail;
        uint64_t data_offset = meta->data_offset;
        uint64_t data_size = meta->data_size;
        const uint8_t *data_base = (const uint8_t *)bases[i] + data_offset;

        /* Sample body: u64 ip; u32 pid,tid; u64 time; u32 cpu, u32 res. */
        const uint64_t cpu_off =
            (uint64_t)sizeof(struct perf_event_header) + 8 + 8 + 8;
        uint64_t samples = 0, wrong_cpu = 0;
        uint64_t off = data_tail;
        while (off < data_head && data_size != 0) {
            uint64_t rel = off % data_size;
            struct perf_event_header hdr;
            ring_copy(data_base, data_size, rel, &hdr, sizeof(hdr));
            if (hdr.size == 0 || off + hdr.size > data_head) {
                break;
            }
            if (hdr.type == PERF_RECORD_SAMPLE && cpu_off + 4 <= hdr.size) {
                uint32_t s_cpu = 0xffffffffu;
                ring_copy(data_base, data_size, (rel + cpu_off) % data_size,
                          &s_cpu, 4);
                samples++;
                if ((int)s_cpu != i) {
                    wrong_cpu++;
                }
            }
            off += hdr.size;
        }

        printf("STARRY_SMP_RECORD_ALLCPU[cpu%d] samples=%llu wrong_cpu=%llu\n", i,
               (unsigned long long)samples, (unsigned long long)wrong_cpu);
        if (samples == 0) {
            printf("smp-record-allcpu FAILED: cpu %d ring empty (not armed on "
                   "target core)\n",
                   i);
            ok = 0;
        } else if (wrong_cpu != 0) {
            printf("smp-record-allcpu FAILED: cpu %d had %llu samples tagged with "
                   "another cpu (armed on wrong core)\n",
                   i, (unsigned long long)wrong_cpu);
            ok = 0;
        }
        (void)munmap(bases[i], PERF_MMAP_TOTAL_BYTES);
        close((int)fds[i]);
    }

    if (ok) {
        printf("STARRY_SMP_RECORD_ALLCPU_OK\n");
        return 0;
    }
    return 1;
}
