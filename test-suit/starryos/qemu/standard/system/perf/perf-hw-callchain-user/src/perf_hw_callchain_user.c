/* User frame-pointer callchain regression: leaf plus three callers.
 *
 * The run pins itself to one CPU and reserves every ordinary programmable PMU
 * counter of that CPU with disabled, non-cycle RAW sampling events. A
 * CPU_CYCLES sampling event then has no ordinary slot left, so it can only
 * deliver user callchains from the native architectural cycle counter. A
 * kernel that models CPU_CYCLES sampling as an ordinary multiplexed slot
 * produces no sample at all and fails here.
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
#include <unistd.h>

#define PERF_TYPE_RAW 4u
#define PERF_SAMPLE_IP (1ull << 0)
#define PERF_SAMPLE_TID (1ull << 1)
#define PERF_SAMPLE_TIME (1ull << 2)
#define PERF_SAMPLE_CALLCHAIN (1ull << 5)
#define PERF_SAMPLE_ID (1ull << 6)
#define PERF_SAMPLE_REGS_USER (1ull << 12)
#define PERF_CONTEXT_USER ((uint64_t)-512)
#define PERF_CONTEXT_MAX ((uint64_t)-4095)
#define PERF_ATTR_FLAG_DISABLED (1ull << 0)
#define PERF_EVENT_IOC_ENABLE 0x2400u
#define PERF_EVENT_IOC_DISABLE 0x2401u
#define PERF_EVENT_IOC_RESET 0x2403u
#define PERF_EVENT_IOC_ID 0x80082407u
#define PERF_RECORD_SAMPLE 9u
#define SYS_PERF_EVENT_OPEN 241
#define RING_BYTES (9u * 4096u)

#if defined(__aarch64__)
/* Architectural CPU_CYCLES and INST_RETIRED event numbers. */
#define ARM_PMU_EVT_CPU_CYCLES 0x11ull
#define ARM_PMU_EVT_INST_RETIRED 0x08ull

/* Non-cycle RAW encodings used to reserve ordinary slots.
 *
 * INST_RETIRED is the mandatory instruction event; the remaining entries only
 * cover a PMU that advertises a narrower common-event set. The encoding is
 * irrelevant to the scenario because these events stay disabled. */
static const uint64_t occupier_events[] = {
    ARM_PMU_EVT_INST_RETIRED,
    0x00ull,
    0x04ull,
};
#define OCCUPIER_EVENT_COUNT \
    (sizeof(occupier_events) / sizeof(occupier_events[0]))

/* One CPU keeps the ordinary-slot pressure and the sampled task colocated. */
#define TARGET_CPU 0
/* PMUv3 implements at most 31 programmable counters per CPU. */
#define MAX_SLOT_OCCUPIERS 31
#endif

struct perf_event_attr_v3 {
    uint32_t type, size;
    uint64_t config, sample_period, sample_type, read_format, flags;
    uint32_t wakeup_events, bp_type;
    uint64_t bp_addr;
    uint64_t config2, branch_sample_type, sample_regs_user;
    uint32_t sample_stack_user, clockid;
};

struct perf_event_mmap_page {
    uint32_t version, compat_version, lock, index;
    int64_t offset;
    uint64_t time_enabled, time_running, capabilities;
    uint16_t pmc_width, time_shift;
    uint32_t time_mult;
    uint64_t time_offset, time_zero;
    uint32_t size, reserved_1;
    uint64_t time_cycles, time_mask;
    uint8_t reserved[928];
    uint64_t data_head, data_tail, data_offset, data_size;
    uint64_t aux_head, aux_tail, aux_offset, aux_size;
};

struct perf_event_header {
    uint32_t type;
    uint16_t misc, size;
};

_Static_assert(sizeof(struct perf_event_attr_v3) == 96, "perf attr v3 size");
_Static_assert(offsetof(struct perf_event_mmap_page, data_head) == 1024,
               "perf data_head offset");

#if defined(__aarch64__)
static volatile uint64_t sink;

/* Bounded diagnostics: the failure summary must stay finite, so at most
 * DIAG_MAX_RECORDS records and DIAG_MAX_ENTRIES chain entries are printed. */
#define DIAG_MAX_RECORDS 10u
#define DIAG_MAX_ENTRIES 8u

__attribute__((noinline)) static void busy(void) {
    /* Keep the sampled execution inside the four FP-linked user frames.
     * Repeated /dev/zero reads mostly sample the kernel and turn this ABI
     * check into a multi-gigabyte copy benchmark under icount. */
    for (uint64_t i = 0; i < 400000; i++) {
        sink += i * 3u + 1u;
    }
}

__attribute__((noinline)) static void inner(void) { busy(); }
__attribute__((noinline)) static void middle(void) { inner(); }
__attribute__((noinline)) static void outer(void) { middle(); }

static void ring_copy(const uint8_t *ring, uint64_t size, uint64_t at,
                      void *dst, size_t len) {
    for (size_t i = 0; i < len; i++) {
        ((uint8_t *)dst)[i] = ring[(at + i) % size];
    }
}

static void cycle_attr(struct perf_event_attr_v3 *attr) {
    struct perf_event_attr_v3 cycle = {
        .type = PERF_TYPE_RAW,
        .size = sizeof(cycle),
        .config = ARM_PMU_EVT_CPU_CYCLES,
        .sample_period = 100000,
        .sample_type = PERF_SAMPLE_IP | PERF_SAMPLE_TID | PERF_SAMPLE_TIME |
                       PERF_SAMPLE_ID | PERF_SAMPLE_CALLCHAIN |
                       PERF_SAMPLE_REGS_USER,
        /* Upstream perf's AArch64 FP unwinder requests LR with this bit. */
        .sample_regs_user = 1ull << 30,
        .flags = PERF_ATTR_FLAG_DISABLED | (1ull << 5), /* exclude_kernel */
    };
    *attr = cycle;
}

/* Pins the whole run to the CPU whose ordinary slots are reserved. */
static int pin_to_target_cpu(void) {
    cpu_set_t affinity;
    CPU_ZERO(&affinity);
    CPU_SET(TARGET_CPU, &affinity);
    if (sched_setaffinity(0, sizeof(affinity), &affinity) != 0) {
        printf("perf-callchain-user FAILED: sched_setaffinity errno=%d\n",
               errno);
        return 1;
    }
    return 0;
}

static void close_slots(const int *fds, int opened) {
    for (int i = 0; i < opened; i++) {
        close(fds[i]);
    }
}

/* Reserves every ordinary programmable counter of TARGET_CPU.
 *
 * An open-time reservation keeps the slot for the fd's lifetime, so disabled
 * system sampling events are enough: nothing is enabled and no sample is
 * produced by this load. The loop stops at the first EBUSY, which is the only
 * error that proves the ordinary capacity is exhausted; any other errno means
 * the pressure precondition itself is broken and must fail.
 */
static int occupy_ordinary_slots(int *fds, int *opened) {
    struct perf_event_attr_v3 attr = {
        .type = PERF_TYPE_RAW,
        .size = sizeof(attr),
        .sample_period = 100000,
        .sample_type = PERF_SAMPLE_IP,
        .flags = PERF_ATTR_FLAG_DISABLED,
    };
    *opened = 0;
    size_t candidate = 0;
    for (; candidate < OCCUPIER_EVENT_COUNT; candidate++) {
        attr.config = occupier_events[candidate];
        long fd = syscall(SYS_PERF_EVENT_OPEN, &attr, -1, TARGET_CPU, -1, 0ul);
        if (fd >= 0) {
            fds[(*opened)++] = (int)fd;
            break;
        }
        int error = errno;
        /* Only an unauthorised encoding may fall through to the next
         * candidate; every other error breaks the pressure precondition. */
        if (error != ENOENT && error != EINVAL) {
            printf("perf-callchain-user FAILED: slot reservation errno=%d\n",
                   error);
            return 1;
        }
    }
    if (candidate == OCCUPIER_EVENT_COUNT) {
        puts("perf-callchain-user FAILED: no usable non-cycle raw event");
        return 1;
    }
    for (int attempt = *opened; attempt < MAX_SLOT_OCCUPIERS; attempt++) {
        long fd = syscall(SYS_PERF_EVENT_OPEN, &attr, -1, TARGET_CPU, -1, 0ul);
        if (fd >= 0) {
            fds[(*opened)++] = (int)fd;
            continue;
        }
        if (errno != EBUSY) {
            printf("perf-callchain-user FAILED: slot reservation errno=%d\n",
                   errno);
            close_slots(fds, *opened);
            *opened = 0;
            return 1;
        }
        printf("DIAG ordinary slots occupied=%d\n", *opened);
        return 0;
    }

    /* Every slot of the bounded array is reserved, which alone cannot decide
     * the precondition: a platform whose ordinary capacity equals
     * MAX_SLOT_OCCUPIERS is still valid, and a larger capacity is not. One
     * further open-only probe separates exhaustion from a short reservation.
     * It never writes `fds`, and a successful probe is closed again. */
    long extra = syscall(SYS_PERF_EVENT_OPEN, &attr, -1, TARGET_CPU, -1, 0ul);
    if (extra >= 0) {
        close((int)extra);
        printf("perf-callchain-user FAILED: %d reservations did not exhaust the "
               "ordinary capacity\n",
               MAX_SLOT_OCCUPIERS);
        close_slots(fds, *opened);
        *opened = 0;
        return 1;
    }
    if (errno != EBUSY) {
        printf("perf-callchain-user FAILED: slot reservation errno=%d\n", errno);
        close_slots(fds, *opened);
        *opened = 0;
        return 1;
    }
    printf("DIAG ordinary slots occupied=%d\n", *opened);
    return 0;
}

/* Reports the counter value seen by read(2) and any bounded read failure. */
static uint64_t event_value(int fd, const char *stage) {
    uint64_t value = 0;
    ssize_t len = read(fd, &value, sizeof(value));
    if (len != (ssize_t)sizeof(value)) {
        printf("DIAG read %s failed: len=%lld errno=%d\n", stage,
               (long long)len, errno);
        return 0;
    }
    printf("DIAG value %s = %llu\n", stage, (unsigned long long)value);
    return value;
}

/* Runs the four-frame user workload under one cycle sampling event. */
static int sample_cycle_callchain(void) {
    struct perf_event_attr_v3 attr;
    cycle_attr(&attr);
    int fd = (int)syscall(SYS_PERF_EVENT_OPEN, &attr, 0, -1, -1, 0ul);
    if (fd < 0) {
        printf("perf-callchain-user FAILED: open errno=%d\n", errno);
        return 1;
    }
    void *mapping =
        mmap(NULL, RING_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (mapping == MAP_FAILED) {
        printf("perf-callchain-user FAILED: mmap errno=%d\n", errno);
        close(fd);
        return 1;
    }
    struct perf_event_mmap_page *meta = mapping;
    uint64_t event_id = 0;
    int failed = 0;
    if (ioctl(fd, PERF_EVENT_IOC_ID, &event_id) != 0 || event_id == 0) {
        puts("perf-callchain-user FAILED: event id");
        failed = 1;
    }
    if (!failed && ioctl(fd, PERF_EVENT_IOC_RESET, 0) != 0) {
        printf("perf-callchain-user FAILED: reset errno=%d\n", errno);
        failed = 1;
    }
    if (!failed && ioctl(fd, PERF_EVENT_IOC_ENABLE, 0) != 0) {
        printf("perf-callchain-user FAILED: enable errno=%d\n", errno);
        failed = 1;
    }
    if (failed) {
        munmap(mapping, RING_BYTES);
        close(fd);
        return 1;
    }
    outer();
    if (ioctl(fd, PERF_EVENT_IOC_DISABLE, 0) != 0) {
        printf("perf-callchain-user FAILED: disable errno=%d\n", errno);
        munmap(mapping, RING_BYTES);
        close(fd);
        return 1;
    }
    /* Sampled after the event stopped, so the read cannot itself change when
     * the next overflow is observed. */
    event_value(fd, "after_disable");

    uint64_t head = meta->data_head;
    __sync_synchronize();
    uint64_t tail = meta->data_tail;
    const uint8_t *ring = (const uint8_t *)mapping + meta->data_offset;
    uint64_t samples = 0, user_chains = 0, max_user_ips = 0, user_regs = 0;
    uint64_t kernel_samples = 0, other_records = 0, records = 0;
    int corrupt = 0;
    printf("DIAG ring id=%llu data_size=%llu head=%llu tail=%llu\n",
           (unsigned long long)event_id, (unsigned long long)meta->data_size,
           (unsigned long long)head, (unsigned long long)tail);
    while (tail < head && meta->data_size != 0) {
        struct perf_event_header header;
        uint64_t start = tail % meta->data_size;
        ring_copy(ring, meta->data_size, start, &header, sizeof(header));
        if (header.size < sizeof(header) || tail + header.size > head) {
            corrupt = 1;
            break;
        }
        records++;
        int dump = records <= DIAG_MAX_RECORDS;
        if (header.type == PERF_RECORD_SAMPLE) {
            samples++;
            /* header, ip, pid/tid, time, id, then callchain nr. */
            uint64_t cursor = 8 + 8 + 8 + 8;
            uint64_t ip = 0, time = 0;
            ring_copy(ring, meta->data_size, start + 8, &ip, 8);
            ring_copy(ring, meta->data_size, start + 24, &time, 8);
            uint64_t sample_id = 0;
            ring_copy(ring, meta->data_size, start + cursor, &sample_id, 8);
            cursor += 8;
            if (sample_id != event_id) {
                corrupt = 1;
                break;
            }
            uint64_t nr = 0;
            if (cursor + 8 > header.size) {
                corrupt = 1;
                break;
            }
            ring_copy(ring, meta->data_size, start + cursor, &nr, 8);
            cursor += 8;
            if (nr > 128 || cursor + nr * 8 > header.size) {
                corrupt = 1;
                break;
            }
            int in_user = 0;
            uint64_t user_ips = 0;
            for (uint64_t i = 0; i < nr; i++) {
                uint64_t entry;
                ring_copy(ring, meta->data_size, start + cursor + i * 8,
                          &entry, 8);
                if (dump && i < DIAG_MAX_ENTRIES) {
                    printf("DIAG rec[%llu] chain[%llu]=0x%llx\n",
                           (unsigned long long)records, (unsigned long long)i,
                           (unsigned long long)entry);
                }
                if (entry >= PERF_CONTEXT_MAX) {
                    in_user = entry == PERF_CONTEXT_USER;
                    if (in_user) {
                        user_chains++;
                    }
                } else if (in_user) {
                    user_ips++;
                }
            }
            if (user_ips > max_user_ips) {
                max_user_ips = user_ips;
            }
            cursor += nr * 8;
            uint64_t abi = 0, lr = 0;
            if (cursor + 8 > header.size) {
                corrupt = 1;
                break;
            }
            ring_copy(ring, meta->data_size, start + cursor, &abi, 8);
            cursor += 8;
            if ((header.misc & 7) == 2) {
                if (abi != 2 || cursor + 8 != header.size) {
                    corrupt = 1;
                    break;
                }
                ring_copy(ring, meta->data_size, start + cursor, &lr, 8);
                if (lr == 0 || lr >= (1ull << 48) || (lr & 3)) {
                    corrupt = 1;
                    break;
                }
                user_regs++;
            } else {
                kernel_samples++;
                if (abi != 0 || cursor != header.size) {
                    corrupt = 1;
                    break;
                }
            }
            if (dump) {
                printf("DIAG rec[%llu] sample misc=%u size=%u ip=0x%llx "
                       "time=%llu nr=%llu abi=%llu lr=0x%llx user_ips=%llu\n",
                       (unsigned long long)records, (unsigned)header.misc,
                       (unsigned)header.size, (unsigned long long)ip,
                       (unsigned long long)time, (unsigned long long)nr,
                       (unsigned long long)abi, (unsigned long long)lr,
                       (unsigned long long)user_ips);
            }
        } else {
            other_records++;
            if (dump) {
                printf("DIAG rec[%llu] type=%u misc=%u size=%u\n",
                       (unsigned long long)records, (unsigned)header.type,
                       (unsigned)header.misc, (unsigned)header.size);
            }
        }
        tail += header.size;
    }

    printf("STARRY_PERF_CALLCHAIN_USER samples=%llu user_chains=%llu "
           "max_user_ips=%llu corrupt=%d kernel_samples=%llu other_records=%llu"
           " records=%llu\n",
           (unsigned long long)samples, (unsigned long long)user_chains,
           (unsigned long long)max_user_ips, corrupt,
           (unsigned long long)kernel_samples, (unsigned long long)other_records,
           (unsigned long long)records);
    munmap(mapping, RING_BYTES);
    close(fd);
    printf("STARRY_PERF_USER_LR samples=%llu\n", (unsigned long long)user_regs);
    if (corrupt || samples == 0 || user_chains == 0 || max_user_ips < 4 ||
        user_regs == 0) {
        puts("perf-callchain-user FAILED: no user callchain with four IPs");
        return 1;
    }
    return 0;
}

/* Proves the native reservation was released when the sampled fd closed.
 *
 * The ordinary slots are held again, so this system-wide sampling reopen can
 * only succeed while the native cycle counter is genuinely free.
 */
static int sampling_release_probe(void) {
    struct perf_event_attr_v3 attr;
    cycle_attr(&attr);
    long fd = syscall(SYS_PERF_EVENT_OPEN, &attr, -1, TARGET_CPU, -1, 0ul);
    if (fd < 0) {
        printf("perf-callchain-user FAILED: cycle counter not released errno=%d"
               "\n",
               errno);
        return 1;
    }
    close((int)fd);
    return 0;
}
#endif

int main(void) {
#if !defined(__aarch64__)
    puts("STARRY_PERF_CALLCHAIN_USER_OK");
    return 0;
#else
    if (pin_to_target_cpu()) {
        return 1;
    }

    int occupiers[MAX_SLOT_OCCUPIERS];
    int occupied = 0;
    if (occupy_ordinary_slots(occupiers, &occupied)) {
        return 1;
    }
    int failed = sample_cycle_callchain();
    close_slots(occupiers, occupied);
    if (!failed) {
        if (occupy_ordinary_slots(occupiers, &occupied)) {
            return 1;
        }
        failed = sampling_release_probe();
        close_slots(occupiers, occupied);
    }
    if (failed) {
        return 1;
    }
    puts("STARRY_PERF_CALLCHAIN_USER_OK");
    return 0;
#endif
}
