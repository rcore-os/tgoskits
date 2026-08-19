/*
 * perf_hw_sample_tid.c -- PERF_RECORD_SAMPLE pid/tid attribution test.
 *
 * Proves each sample carries the REAL userspace (tgid, tid) -- the same ids the
 * COMM/MMAP2 side-band records use -- so `perf report` joins a sample to its
 * process/DSO map correctly. Historically the sample pid/tid were derived from
 * the axtask *scheduler* id at IRQ time (pid == tid == scheduler id), which is
 * wrong for a multithreaded process: a non-leader thread's samples reported
 * pid == thread-tid instead of the shared process tgid, so they failed to join
 * the process maps in perf report.
 *
 * Why a NON-leader thread: for a single-threaded process the scheduler id
 * happens to equal both the tid and the tgid (the userspace tid is derived from
 * the axtask id at clone, and the leader's tid == getpid()), so a self-sample
 * would pass even with the buggy derivation. Only a worker thread, whose
 * gettid() differs from the process getpid(), exposes the tgid/tid distinction.
 *
 * Flow: a worker pthread opens a per-task sampling event on ITSELF (pid = 0),
 * mmaps the ring, ENABLEs, runs a syscall-heavy read(/dev/zero) loop (QEMU-TCG's
 * cycle counter barely advances on pure-ALU loops, so real syscalls are needed
 * to make the counter overflow and produce samples), DISABLEs, then parses every
 * PERF_RECORD_SAMPLE body (u64 ip; u32 pid; u32 tid; u64 time).
 *
 * SUCCESS ==
 *     fd >= 0 AND mmap ok AND the ring is non-empty
 *   AND worker gettid() != process getpid()   (we are genuinely multithreaded)
 *   AND every sample reports tid == worker gettid()
 *   AND every sample reports pid == process getpid()  (the shared tgid, NOT the
 *       per-thread tid) -- this is the property the fix establishes.
 * On success exactly one line `STARRY_PERF_SAMPLE_TID_OK` is printed.
 *
 * All ABI structs are defined locally (no <linux/perf_event.h> dependency).
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
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
    uint32_t version;
    uint32_t compat_version;
    uint32_t lock;
    uint32_t index;
    int64_t offset;
    uint64_t time_enabled;
    uint64_t time_running;
    union {
        uint64_t capabilities;
        struct {
            uint64_t cap_bit0 : 1, cap_bit0_is_deprecated : 1,
                cap_user_rdpmc : 1, cap_user_time : 1, cap_user_time_zero : 1,
                cap_user_time_short : 1, cap_____res : 58;
        };
    };
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
    uint64_t aux_head;
    uint64_t aux_tail;
    uint64_t aux_offset;
    uint64_t aux_size;
};

_Static_assert(offsetof(struct perf_event_attr, sample_period) == 16, "off16");
_Static_assert(offsetof(struct perf_event_attr, sample_type) == 24, "off24");
_Static_assert(offsetof(struct perf_event_mmap_page, data_head) == 1024, "dh");
_Static_assert(offsetof(struct perf_event_mmap_page, data_tail) == 1032, "dt");
_Static_assert(offsetof(struct perf_event_mmap_page, data_offset) == 1040, "do");
_Static_assert(offsetof(struct perf_event_mmap_page, data_size) == 1048, "ds");

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

static volatile uint64_t g_sink;
static int g_zfd = -1;

/* Syscall-heavy workload so the QEMU-TCG cycle counter actually overflows. */
static void busy(void) {
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

/* Result the worker thread hands back to main (populated before pthread_join,
 * read after -- the join is the happens-before edge, so no atomics needed). */
struct worker_result {
    int failed;      /* non-zero => failure; `reason` explains */
    char reason[96]; /* failure detail for the FAILED line */
    pid_t worker_tid;
    pid_t proc_pid;
    uint64_t samples;
    uint64_t bad_pid; /* samples whose pid != process getpid() (the bug) */
    uint64_t bad_tid; /* samples whose tid != worker gettid() */
};

static void wr_fail(struct worker_result *wr, const char *reason) {
    wr->failed = 1;
    snprintf(wr->reason, sizeof(wr->reason), "%s", reason);
}

static void *worker(void *arg) {
    struct worker_result *wr = (struct worker_result *)arg;
    wr->worker_tid = (pid_t)syscall(SYS_gettid);
    wr->proc_pid = getpid();

    struct perf_event_attr attr;
    for (size_t i = 0; i < sizeof(attr); i++) {
        ((volatile unsigned char *)&attr)[i] = 0;
    }
    attr.type = PERF_TYPE_RAW;
    attr.config = ARM_PMU_EVT_CPU_CYCLES;
    attr.size = (uint32_t)sizeof(struct perf_event_attr);
    attr.sample_period = SAMPLE_PERIOD;
    attr.sample_type = PERF_SAMPLE_IP | PERF_SAMPLE_TID | PERF_SAMPLE_TIME;
    attr.read_format = 0;
    attr.flags = PERF_ATTR_FLAG_DISABLED;

    /* Target the worker's own tid (pid > 0) to exercise the PER-TASK sampling
     * path: this kernel routes pid == 0 to the self/system-wide path (per-task is
     * pid > 0), which would attribute kernel-context samples to whatever kernel
     * task is current rather than to this thread. A real `perf record -- cmd`
     * likewise opens per-task events on the child's pid. */
    long fd = perf_event_open(&attr, wr->worker_tid, -1, -1, 0ul);
    if (fd < 0) {
        char msg[96];
        snprintf(msg, sizeof(msg), "perf_event_open errno=%d", errno);
        wr_fail(wr, msg);
        return NULL;
    }
    int efd = (int)fd;

    void *base = mmap(NULL, PERF_MMAP_TOTAL_BYTES, PROT_READ | PROT_WRITE,
                      MAP_SHARED, efd, 0);
    if (base == MAP_FAILED) {
        char msg[96];
        snprintf(msg, sizeof(msg), "mmap ring errno=%d", errno);
        close(efd);
        wr_fail(wr, msg);
        return NULL;
    }
    struct perf_event_mmap_page *meta = (struct perf_event_mmap_page *)base;

    g_zfd = open("/dev/zero", O_RDONLY);

    (void)ioctl(efd, PERF_EVENT_IOC_RESET, 0);
    (void)ioctl(efd, PERF_EVENT_IOC_ENABLE, 0);
    busy();
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

    uint64_t off = data_tail;
    while (off < data_head && data_size != 0) {
        uint64_t rel = off % data_size;
        struct perf_event_header hdr;
        ring_copy(data_base, data_size, rel, &hdr, sizeof(hdr));
        if (hdr.size == 0 || off + hdr.size > data_head) {
            break;
        }
        if (hdr.type == PERF_RECORD_SAMPLE) {
            /* body: u64 ip; u32 pid; u32 tid; u64 time */
            uint64_t pid_off = (uint64_t)sizeof(hdr) + 8;
            if (pid_off + 8 <= hdr.size) {
                uint32_t s_pid = 0, s_tid = 0;
                ring_copy(data_base, data_size, (rel + pid_off) % data_size,
                          &s_pid, 4);
                ring_copy(data_base, data_size, (rel + pid_off + 4) % data_size,
                          &s_tid, 4);
                wr->samples++;
                if ((pid_t)s_pid != wr->proc_pid) {
                    wr->bad_pid++;
                }
                if ((pid_t)s_tid != wr->worker_tid) {
                    wr->bad_tid++;
                }
            }
        }
        off += hdr.size;
    }

    if (data_head == data_tail) {
        wr_fail(wr, "no samples captured (data_head == data_tail)");
    } else if (wr->samples == 0) {
        wr_fail(wr, "no PERF_RECORD_SAMPLE records in ring");
    }

    (void)munmap(base, PERF_MMAP_TOTAL_BYTES);
    close(efd);
    return NULL;
}

int main(void) {
#if !defined(__aarch64__)
    /* Hardware-PMU perf is aarch64-only (ARM PMUv3); skip-as-pass elsewhere. */
    printf("STARRY_PERF_SAMPLE_TID_OK\n");
    return 0;
#endif
    struct worker_result wr;
    for (size_t i = 0; i < sizeof(wr); i++) {
        ((volatile unsigned char *)&wr)[i] = 0;
    }

    pthread_t th;
    if (pthread_create(&th, NULL, worker, &wr) != 0) {
        printf("perf-sample-tid FAILED: pthread_create\n");
        return 1;
    }
    (void)pthread_join(th, NULL);

    printf("STARRY_PERF_SAMPLE_TID pid=%d worker_tid=%d samples=%llu "
           "bad_pid=%llu bad_tid=%llu\n",
           (int)wr.proc_pid, (int)wr.worker_tid,
           (unsigned long long)wr.samples, (unsigned long long)wr.bad_pid,
           (unsigned long long)wr.bad_tid);

    if (wr.failed) {
        printf("perf-sample-tid FAILED: %s\n", wr.reason);
        return 1;
    }
    if (wr.worker_tid == wr.proc_pid) {
        printf("perf-sample-tid FAILED: worker tid == getpid (not a non-leader "
               "thread; test cannot distinguish tgid from tid)\n");
        return 1;
    }
    if (wr.bad_tid != 0) {
        printf("perf-sample-tid FAILED: %llu samples had tid != worker gettid\n",
               (unsigned long long)wr.bad_tid);
        return 1;
    }
    if (wr.bad_pid != 0) {
        /* The historical bug: pid was the per-thread scheduler id, not the
         * shared process tgid, so samples could not join the process maps. */
        printf("perf-sample-tid FAILED: %llu samples had pid != process getpid "
               "(sample not attributed to the process tgid)\n",
               (unsigned long long)wr.bad_pid);
        return 1;
    }

    printf("STARRY_PERF_SAMPLE_TID_OK\n");
    return 0;
}
