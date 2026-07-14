/*
 * perf_hw_group_sample.c -- group-leader sampling (PERF_SAMPLE_READ|GROUP).
 *
 * When a PER-TASK sampling event is a group LEADER opened with
 * read_format = PERF_FORMAT_GROUP | PERF_FORMAT_ID, each PERF_RECORD_SAMPLE must
 * carry the WHOLE group's counters -- the Linux group read layout, assembled
 * from the hard-IRQ overflow handler:
 *
 *     u64 nr;                       // number of events (leader + members)
 *     { u64 value; u64 id; } [nr];  // leader first, then each member
 *
 * The leader's value is its synthetic period-advanced running count; each
 * member's value is its live count (accumulated + the in-progress slice, read
 * from the member's banked PMU counter on the sampling core).
 *
 * Flow: a worker pthread opens a group on ITSELF (pid = worker gettid, the
 * per-task path):
 *   - leader  : CPU_CYCLES SAMPLING, sample_type = IP|TID|TIME|READ,
 *               read_format = GROUP|ID, disabled.
 *   - member  : CPU_CYCLES COUNTING (sample_period = 0), group_fd = leader.
 * It mmaps the leader ring, ENABLEs the leader (which starts the whole group),
 * runs a syscall-heavy read(/dev/zero) loop so the QEMU-TCG cycle counter
 * overflows and produces samples, DISABLEs, then parses every sample's group
 * read block.
 *
 * SUCCESS ==
 *     both fds >= 0 AND mmap ok AND >= 2 PERF_RECORD_SAMPLE records
 *   AND every sample reports nr == 2 (leader + member)
 *   AND the leader id / member id in every sample match PERF_EVENT_IOC_ID
 *       and are distinct (the group demultiplexes correctly)
 *   AND the leader value is strictly increasing and non-zero (running count)
 *   AND every member value is non-zero (its live count was read from HW).
 * On success exactly one line `STARRY_PERF_GROUP_SAMPLE_OK` is printed.
 *
 * aarch64-only (ARM PMUv3); skips-as-pass elsewhere.
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
#define PERF_SAMPLE_READ (1ull << 10)

/* read_format bits. */
#define PERF_FORMAT_ID (1ull << 2)
#define PERF_FORMAT_GROUP (1ull << 3)

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
/* _IOR('$', 7, __u64 *); the kernel matches on the ('$', 7) pair. */
#ifndef PERF_EVENT_IOC_ID
#define PERF_EVENT_IOC_ID 0x80082407u
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

static void attr_init(struct perf_event_attr *attr) {
    for (size_t i = 0; i < sizeof(*attr); i++) {
        ((volatile unsigned char *)attr)[i] = 0;
    }
    attr->type = PERF_TYPE_RAW;
    attr->config = ARM_PMU_EVT_CPU_CYCLES;
    attr->size = (uint32_t)sizeof(*attr);
}

/* Result the worker hands back to main; read after pthread_join. */
struct worker_result {
    int failed;
    char reason[112];
    uint64_t samples;
    uint64_t bad_nr;     /* samples whose group nr != 2 */
    uint64_t bad_id;     /* samples whose leader/member id mismatched IOC_ID */
    uint64_t bad_order;  /* leader value not strictly increasing */
    uint64_t zero_lead;  /* leader value == 0 */
    uint64_t zero_memb;  /* member value == 0 */
    uint64_t last_lead;  /* last leader value seen */
    uint64_t last_memb;  /* last member value seen */
};

static void wr_fail(struct worker_result *wr, const char *reason) {
    wr->failed = 1;
    snprintf(wr->reason, sizeof(wr->reason), "%s", reason);
}

static void *worker(void *arg) {
    struct worker_result *wr = (struct worker_result *)arg;
    pid_t self = (pid_t)syscall(SYS_gettid);

    /* Leader: CPU_CYCLES sampling, group read layout with per-event ids. */
    struct perf_event_attr lattr;
    attr_init(&lattr);
    lattr.sample_period = SAMPLE_PERIOD;
    lattr.sample_type =
        PERF_SAMPLE_IP | PERF_SAMPLE_TID | PERF_SAMPLE_TIME | PERF_SAMPLE_READ;
    lattr.read_format = PERF_FORMAT_GROUP | PERF_FORMAT_ID;
    lattr.flags = PERF_ATTR_FLAG_DISABLED;

    long lfd = perf_event_open(&lattr, self, -1, -1, 0ul);
    if (lfd < 0) {
        char msg[112];
        snprintf(msg, sizeof(msg), "leader perf_event_open errno=%d", errno);
        wr_fail(wr, msg);
        return NULL;
    }
    int leader = (int)lfd;

    /* Member: CPU_CYCLES counting, in the leader's group. */
    struct perf_event_attr mattr;
    attr_init(&mattr);
    mattr.sample_period = 0;
    mattr.read_format = PERF_FORMAT_GROUP | PERF_FORMAT_ID;
    mattr.flags = PERF_ATTR_FLAG_DISABLED;

    long mfd = perf_event_open(&mattr, self, -1, leader, 0ul);
    if (mfd < 0) {
        char msg[112];
        snprintf(msg, sizeof(msg), "member perf_event_open errno=%d", errno);
        close(leader);
        wr_fail(wr, msg);
        return NULL;
    }
    int member = (int)mfd;

    /* The ids the group read block must carry, per event. */
    uint64_t leader_id = 0, member_id = 0;
    if (ioctl(leader, PERF_EVENT_IOC_ID, &leader_id) != 0 ||
        ioctl(member, PERF_EVENT_IOC_ID, &member_id) != 0) {
        char msg[112];
        snprintf(msg, sizeof(msg), "PERF_EVENT_IOC_ID errno=%d", errno);
        close(member);
        close(leader);
        wr_fail(wr, msg);
        return NULL;
    }
    if (leader_id == member_id) {
        close(member);
        close(leader);
        wr_fail(wr, "leader id == member id (ids not distinct)");
        return NULL;
    }

    void *base = mmap(NULL, PERF_MMAP_TOTAL_BYTES, PROT_READ | PROT_WRITE,
                      MAP_SHARED, leader, 0);
    if (base == MAP_FAILED) {
        char msg[112];
        snprintf(msg, sizeof(msg), "mmap ring errno=%d", errno);
        close(member);
        close(leader);
        wr_fail(wr, msg);
        return NULL;
    }
    struct perf_event_mmap_page *meta = (struct perf_event_mmap_page *)base;

    g_zfd = open("/dev/zero", O_RDONLY);
    (void)ioctl(leader, PERF_EVENT_IOC_RESET, 0);
    /* Enabling the leader starts the whole group (members follow). */
    (void)ioctl(leader, PERF_EVENT_IOC_ENABLE, 0);
    workload();
    (void)ioctl(leader, PERF_EVENT_IOC_DISABLE, 0);
    if (g_zfd >= 0) {
        close(g_zfd);
    }

    uint64_t data_head = meta->data_head;
    __sync_synchronize();
    uint64_t data_tail = meta->data_tail;
    uint64_t data_offset = meta->data_offset;
    uint64_t data_size = meta->data_size;
    const uint8_t *data_base = (const uint8_t *)base + data_offset;

    /* Sample body: u64 ip; u32 pid,tid; u64 time; then the group read block:
     *   u64 nr; { u64 value; u64 id; }[nr]   (read_format = GROUP|ID). */
    const uint64_t read_off = (uint64_t)sizeof(struct perf_event_header) + 8 + 8 + 8;
    uint64_t off = data_tail;
    while (off < data_head && data_size != 0) {
        uint64_t rel = off % data_size;
        struct perf_event_header hdr;
        ring_copy(data_base, data_size, rel, &hdr, sizeof(hdr));
        if (hdr.size == 0 || off + hdr.size > data_head) {
            break;
        }
        /* Need nr (8) + 2 entries * 16 bytes = 40 bytes of read block. */
        if (hdr.type == PERF_RECORD_SAMPLE && read_off + 40 <= hdr.size) {
            uint64_t nr = 0, lval = 0, lid = 0, mval = 0, mid = 0;
            ring_copy(data_base, data_size, (rel + read_off) % data_size, &nr, 8);
            ring_copy(data_base, data_size, (rel + read_off + 8) % data_size,
                      &lval, 8);
            ring_copy(data_base, data_size, (rel + read_off + 16) % data_size,
                      &lid, 8);
            ring_copy(data_base, data_size, (rel + read_off + 24) % data_size,
                      &mval, 8);
            ring_copy(data_base, data_size, (rel + read_off + 32) % data_size,
                      &mid, 8);
            wr->samples++;
            if (nr != 2) {
                wr->bad_nr++;
            }
            if (lid != leader_id || mid != member_id) {
                wr->bad_id++;
            }
            if (lval == 0) {
                wr->zero_lead++;
            }
            if (mval == 0) {
                wr->zero_memb++;
            }
            if (wr->samples > 1 && lval <= wr->last_lead) {
                wr->bad_order++;
            }
            wr->last_lead = lval;
            wr->last_memb = mval;
        }
        off += hdr.size;
    }

    (void)munmap(base, PERF_MMAP_TOTAL_BYTES);
    close(member);
    close(leader);
    return NULL;
}

int main(void) {
#if !defined(__aarch64__)
    printf("STARRY_PERF_GROUP_SAMPLE_OK\n");
    return 0;
#endif
    struct worker_result wr;
    for (size_t i = 0; i < sizeof(wr); i++) {
        ((volatile unsigned char *)&wr)[i] = 0;
    }

    pthread_t th;
    if (pthread_create(&th, NULL, worker, &wr) != 0) {
        printf("perf-group-sample FAILED: pthread_create\n");
        return 1;
    }
    (void)pthread_join(th, NULL);

    printf("STARRY_PERF_GROUP_SAMPLE samples=%llu bad_nr=%llu bad_id=%llu "
           "bad_order=%llu zero_lead=%llu zero_memb=%llu last_lead=%llu "
           "last_memb=%llu\n",
           (unsigned long long)wr.samples, (unsigned long long)wr.bad_nr,
           (unsigned long long)wr.bad_id, (unsigned long long)wr.bad_order,
           (unsigned long long)wr.zero_lead, (unsigned long long)wr.zero_memb,
           (unsigned long long)wr.last_lead, (unsigned long long)wr.last_memb);

    if (wr.failed) {
        printf("perf-group-sample FAILED: %s\n", wr.reason);
        return 1;
    }
    if (wr.samples < 2) {
        printf("perf-group-sample FAILED: fewer than 2 samples\n");
        return 1;
    }
    if (wr.bad_nr != 0) {
        printf("perf-group-sample FAILED: %llu samples had group nr != 2\n",
               (unsigned long long)wr.bad_nr);
        return 1;
    }
    if (wr.bad_id != 0) {
        printf("perf-group-sample FAILED: %llu samples had wrong leader/member "
               "id (group demux broken)\n",
               (unsigned long long)wr.bad_id);
        return 1;
    }
    if (wr.zero_lead != 0) {
        printf("perf-group-sample FAILED: %llu samples had leader value 0\n",
               (unsigned long long)wr.zero_lead);
        return 1;
    }
    if (wr.bad_order != 0) {
        printf("perf-group-sample FAILED: %llu samples had leader value not "
               "strictly increasing (running count broken)\n",
               (unsigned long long)wr.bad_order);
        return 1;
    }
    if (wr.zero_memb != 0) {
        printf("perf-group-sample FAILED: %llu samples had member value 0 "
               "(member live count not read in the overflow handler)\n",
               (unsigned long long)wr.zero_memb);
        return 1;
    }

    printf("STARRY_PERF_GROUP_SAMPLE_OK\n");
    return 0;
}
