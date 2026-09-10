/* Linux v7.1 leader-first PERF_SAMPLE_READ group regression. */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <sched.h>
#include <time.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#define PERF_TYPE_RAW 4u
#define PERF_SAMPLE_IP (1ull << 0)
#define PERF_SAMPLE_READ (1ull << 4)
#define PERF_FORMAT_ID (1ull << 2)
#define PERF_FORMAT_GROUP (1ull << 3)
#define PERF_ATTR_FLAG_DISABLED (1ull << 0)
#define PERF_EVENT_IOC_ENABLE 0x2400u
#define PERF_EVENT_IOC_DISABLE 0x2401u
#define PERF_EVENT_IOC_RESET 0x2403u
#define PERF_EVENT_IOC_ID 0x80082407u
#define PERF_RECORD_SAMPLE 9u
#define SYS_PERF_EVENT_OPEN 241
#define RING_BYTES (9u * 4096u)

struct perf_event_attr_v0 {
    uint32_t type, size;
    uint64_t config, sample_period, sample_type, read_format, flags;
    uint32_t wakeup_events, bp_type;
    uint64_t bp_addr;
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

_Static_assert(sizeof(struct perf_event_attr_v0) == 64, "perf attr v0 size");
_Static_assert(offsetof(struct perf_event_mmap_page, data_head) == 1024,
               "perf data_head offset");

#if defined(__aarch64__)
static volatile uint64_t sink;

static int check_reset_times(uint32_t type, uint64_t config, int pid, int cpu) {
    struct perf_event_attr_v0 attr = {
        .type = type, .size = sizeof(attr), .config = config,
        .read_format = 3, .flags = PERF_ATTR_FLAG_DISABLED,
    };
    int fd = syscall(SYS_PERF_EVENT_OPEN, &attr, pid, cpu, -1, 0ul);
    if (fd < 0 || ioctl(fd, PERF_EVENT_IOC_ENABLE, 0)) return 1;
    uint64_t before[3] = {0}, after[3] = {0};
    for (int retry = 0; retry < 2000 && before[2] == 0; ++retry) {
        for (uint64_t i = 0; i < 10000; ++i) sink += i;
        sched_yield();
        if (read(fd, before, sizeof(before)) != sizeof(before)) return 1;
    }
    if (ioctl(fd, PERF_EVENT_IOC_DISABLE, 0) ||
        read(fd, before, sizeof(before)) != sizeof(before) ||
        ioctl(fd, PERF_EVENT_IOC_RESET, 0) ||
        read(fd, after, sizeof(after)) != sizeof(after)) return 1;
    close(fd);
    printf("reset-times type=%u config=%llu before=%llu/%llu after=%llu/%llu value=%llu\n",
           type, (unsigned long long)config, (unsigned long long)before[1],
           (unsigned long long)before[2], (unsigned long long)after[1],
           (unsigned long long)after[2], (unsigned long long)after[0]);
    return before[1] == 0 || before[2] == 0 || after[0] != 0 ||
           after[1] != before[1] || after[2] != before[2];
}

static int check_large_period(void) {
    struct perf_event_attr_v0 attr = {
        .type = PERF_TYPE_RAW, .size = sizeof(attr), .config = 0x11,
        .sample_period = UINT32_MAX, .sample_type = PERF_SAMPLE_IP | (1ull << 8),
        .flags = PERF_ATTR_FLAG_DISABLED,
    };
    int fd = syscall(SYS_PERF_EVENT_OPEN, &attr, -1, sched_getcpu(), -1, 0ul);
    if (fd < 0) return 1;
    void *mapping = mmap(NULL, RING_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (mapping == MAP_FAILED || ioctl(fd, PERF_EVENT_IOC_ENABLE, 0)) return 1;
    struct perf_event_mmap_page *meta = mapping;
    struct timespec start, now;
    if (clock_gettime(CLOCK_MONOTONIC, &start)) return 1;
    /* Do not read the perf fd before the first sample: live reads would split
     * the raw delta themselves and conceal a missing overflow extension. */
    while (__atomic_load_n(&meta->data_head, __ATOMIC_ACQUIRE) == 0) {
        if (clock_gettime(CLOCK_MONOTONIC, &now) || now.tv_sec - start.tv_sec > 12) {
            puts("perf-group-sample FAILED: large-period sample deadline");
            return 1;
        }
        sched_yield();
    }
    uint64_t value = 0;
    if (ioctl(fd, PERF_EVENT_IOC_DISABLE, 0) || read(fd, &value, sizeof(value)) != sizeof(value)) return 1;
    const uint64_t *record = (const uint64_t *)((const uint8_t *)mapping + meta->data_offset);
    struct perf_event_header header = *(const struct perf_event_header *)record;
    int failed = header.type != PERF_RECORD_SAMPLE || header.size != 24 ||
                 record[2] != UINT32_MAX || value < UINT32_MAX;
    printf("large-period value=%llu period=%llu\n", (unsigned long long)value,
           (unsigned long long)record[2]);
    munmap(mapping, RING_BYTES);
    close(fd);
    return failed;
}

static int sample_values_valid(uint64_t leader, uint64_t previous_leader,
                               uint64_t member, uint64_t previous_member,
                               int sampling_member) {
    /* Only the leader caused this overflow. A sibling is a cumulative read,
     * not an independent promise of progress for every leader sample. */
    return leader > previous_leader && member >= previous_member &&
           (!sampling_member || member < UINT32_MAX);
}

/* A sampling event must count before its first overflow, including its last
 * partial slice. A huge period keeps this independent of sample delivery. */
static int check_partial_period(int system_wide) {
    struct perf_event_attr_v0 attr = {
        .type = PERF_TYPE_RAW, .size = sizeof(attr), .config = 0x11,
        .sample_period = UINT32_MAX, .sample_type = PERF_SAMPLE_IP,
        .flags = PERF_ATTR_FLAG_DISABLED,
    };
    int fd = syscall(SYS_PERF_EVENT_OPEN, &attr, system_wide ? -1 : 0,
                     system_wide ? sched_getcpu() : -1, -1, 0ul);
    if (fd < 0) return 1;
    void *mapping = mmap(NULL, RING_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (mapping == MAP_FAILED) return 1;
    if (ioctl(fd, PERF_EVENT_IOC_ENABLE, 0)) return 1;
    for (uint64_t i = 0; i < 100000; ++i) sink += i;
    uint64_t live = 0, stopped = 0;
    if (read(fd, &live, sizeof(live)) != sizeof(live) ||
        ioctl(fd, PERF_EVENT_IOC_DISABLE, 0) ||
        read(fd, &stopped, sizeof(stopped)) != sizeof(stopped)) return 1;
    struct perf_event_mmap_page *meta = mapping;
    int failed = live == 0 || stopped < live || stopped >= UINT32_MAX ||
                 meta->data_head != 0;
    printf("partial-period system=%d live=%llu stopped=%llu head=%llu\n",
           system_wide, (unsigned long long)live, (unsigned long long)stopped,
           (unsigned long long)meta->data_head);
    uint64_t reset_value = UINT64_MAX, resumed = 0;
    if (ioctl(fd, PERF_EVENT_IOC_RESET, 0) ||
        read(fd, &reset_value, sizeof(reset_value)) != sizeof(reset_value) ||
        reset_value != 0 || ioctl(fd, PERF_EVENT_IOC_ENABLE, 0)) failed = 1;
    for (uint64_t i = 0; i < 100000; ++i) sink += i;
    if (ioctl(fd, PERF_EVENT_IOC_DISABLE, 0) ||
        read(fd, &resumed, sizeof(resumed)) != sizeof(resumed) ||
        resumed == 0 || resumed >= UINT32_MAX) failed = 1;
    munmap(mapping, RING_BYTES);
    close(fd);
    return failed;
}

static void ring_copy(const uint8_t *ring, uint64_t size, uint64_t at,
                      void *dst, size_t len) {
    for (size_t i = 0; i < len; i++) {
        ((uint8_t *)dst)[i] = ring[(at + i) % size];
    }
}
#endif

static int check_group_sample(int sampling_member) {
#if !defined(__aarch64__)
    (void)sampling_member;
    return 0;
#else
    struct perf_event_attr_v0 leader_attr = {
        .type = PERF_TYPE_RAW,
        .size = sizeof(leader_attr),
        .config = 0x11,
        .sample_period = 100000,
        .sample_type = PERF_SAMPLE_IP | PERF_SAMPLE_READ,
        .read_format = PERF_FORMAT_GROUP | PERF_FORMAT_ID,
        .flags = PERF_ATTR_FLAG_DISABLED,
    };
    int leader = (int)syscall(SYS_PERF_EVENT_OPEN, &leader_attr, 0, -1, -1, 0ul);
    if (leader < 0) {
        printf("perf-group-sample FAILED: leader open errno=%d\n", errno);
        return 1;
    }
    struct perf_event_attr_v0 member_attr = {
        .type = PERF_TYPE_RAW,
        .size = sizeof(member_attr),
        .config = 0x11,
        .sample_period = sampling_member ? UINT32_MAX : 0,
        .sample_type = sampling_member ? PERF_SAMPLE_IP : 0,
        /* Linux groups are gated by the disabled leader; siblings stay enabled. */
    };
    int member =
        (int)syscall(SYS_PERF_EVENT_OPEN, &member_attr, 0, -1, leader, 0ul);
    if (member < 0) {
        printf("perf-group-sample FAILED: member open errno=%d\n", errno);
        close(leader);
        return 1;
    }
    uint64_t leader_id = 0, member_id = 0;
    if (ioctl(leader, PERF_EVENT_IOC_ID, &leader_id) != 0 ||
        ioctl(member, PERF_EVENT_IOC_ID, &member_id) != 0) {
        puts("perf-group-sample FAILED: read ids");
        close(member);
        close(leader);
        return 1;
    }
    void *mapping = mmap(NULL, RING_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED,
                         leader, 0);
    if (mapping == MAP_FAILED) {
        printf("perf-group-sample FAILED: mmap errno=%d\n", errno);
        close(member);
        close(leader);
        return 1;
    }
    struct perf_event_mmap_page *meta = mapping;
    void *member_mapping = MAP_FAILED;
    if (sampling_member) {
        member_mapping = mmap(NULL, RING_BYTES, PROT_READ | PROT_WRITE,
                              MAP_SHARED, member, 0);
        if (member_mapping == MAP_FAILED) return 1;
    }
    if (ioctl(leader, PERF_EVENT_IOC_RESET, 0) ||
        ioctl(leader, PERF_EVENT_IOC_ENABLE, 0)) return 1;
    struct timespec start, now;
    if (clock_gettime(CLOCK_MONOTONIC, &start)) return 1;
    for (;;) {
        for (uint64_t i = 0; i < 1000; i++) sink += (i ^ (sink >> 1)) + 3;
        uint64_t progress[5];
        if (read(leader, progress, sizeof(progress)) != sizeof(progress)) return 1;
        /* Bound the real PMU work, not source iterations: after retaining
         * period_left across slices a long workload legitimately overflows
         * even UINT32_MAX. A hundred leader periods suffice for live READ. */
        if (progress[1] >= 100 * leader_attr.sample_period) break;
        if (clock_gettime(CLOCK_MONOTONIC, &now) ||
            now.tv_sec - start.tv_sec >= 10) return 1;
    }
    if (ioctl(leader, PERF_EVENT_IOC_DISABLE, 0)) return 1;

    uint64_t head = meta->data_head;
    __sync_synchronize();
    uint64_t tail = meta->data_tail;
    const uint8_t *ring = (const uint8_t *)mapping + meta->data_offset;
    uint64_t samples = 0, last_leader = 0, last_member = 0;
    uint64_t first_member = 0;
    int corrupt = 0;
    while (tail < head && meta->data_size != 0) {
        struct perf_event_header header;
        uint64_t start = tail % meta->data_size;
        ring_copy(ring, meta->data_size, start, &header, sizeof(header));
        if (header.size < sizeof(header) || tail + header.size > head) {
            corrupt = 1;
            break;
        }
        if (header.type == PERF_RECORD_SAMPLE) {
            /* header, IP, nr, then leader(value,id), member(value,id). */
            uint64_t fields[5] = {0};
            if (header.size < 56) {
                corrupt = 1;
                break;
            }
            ring_copy(ring, meta->data_size, start + 16, fields, sizeof(fields));
            if (fields[0] != 2 || fields[2] != leader_id ||
                fields[4] != member_id ||
                !sample_values_valid(fields[1], last_leader, fields[3],
                                     last_member, sampling_member)) {
                printf("STARRY_PERF_GROUP_SAMPLE_BAD nr=%llu leader=%llu/%llu "
                       "leader_id=%llu/%llu member=%llu/%llu member_id=%llu/%llu\n",
                       (unsigned long long)fields[0],
                       (unsigned long long)fields[1],
                       (unsigned long long)last_leader,
                       (unsigned long long)fields[2],
                       (unsigned long long)leader_id,
                       (unsigned long long)fields[3],
                       (unsigned long long)last_member,
                       (unsigned long long)fields[4],
                       (unsigned long long)member_id);
                corrupt = 1;
                break;
            }
            if (samples == 0) first_member = fields[3];
            last_leader = fields[1];
            last_member = fields[3];
            samples++;
        }
        tail += header.size;
    }
    printf("STARRY_PERF_GROUP_SAMPLE samples=%llu leader=%llu member=%llu corrupt=%d\n",
           (unsigned long long)samples, (unsigned long long)last_leader,
           (unsigned long long)last_member, corrupt);
    uint64_t final[5] = {0};
    if (read(leader, final, sizeof(final)) != sizeof(final) || final[0] != 2 ||
        final[1] < last_leader || final[3] < last_member) {
        printf("group-final sampling=%d nr=%llu leader=%llu/%llu member=%llu/%llu\n",
               sampling_member, (unsigned long long)final[0],
               (unsigned long long)final[1], (unsigned long long)last_leader,
               (unsigned long long)final[3], (unsigned long long)last_member);
        corrupt = 1;
    }
    if (sampling_member) {
        /* The member never overflows: its group READ must still increase. */
        struct perf_event_mmap_page *member_meta = member_mapping;
        if (member_meta->data_head != 0 || last_member >= UINT32_MAX ||
            final[3] >= UINT32_MAX) {
            printf("group-member head=%llu value=%llu final=%llu\n",
                   (unsigned long long)member_meta->data_head,
                   (unsigned long long)last_member,
                   (unsigned long long)final[3]);
            corrupt = 1;
        }
        munmap(member_mapping, RING_BYTES);
    }
    munmap(mapping, RING_BYTES);
    close(member);
    close(leader);
    /* Plateaus may occur between adjacent snapshots, but a permanently frozen
     * member must still fail over the complete workload. */
    if (corrupt || samples < 2 || last_leader == 0 || last_member <= first_member) {
        puts("perf-group-sample FAILED: malformed or empty group snapshot");
        return 1;
    }
    return 0;
#endif
}

int main(void) {
#if defined(__aarch64__)
    int reset_failures = check_reset_times(PERF_TYPE_RAW, 0x11, -1, sched_getcpu());
    reset_failures += check_reset_times(1, 1, 0, -1);
    reset_failures += check_reset_times(1, 0, -1, sched_getcpu());
    if (reset_failures) {
        puts("perf-group-sample FAILED: RESET changed timing fields");
        return 1;
    }
    if (check_large_period()) {
        puts("perf-group-sample FAILED: large logical period accounting");
        return 1;
    }
    /* Replay the adjacent snapshots from CI job 102711616406. A group member
     * is not itself the overflow source: equal adjacent snapshots are valid. */
    if (!sample_values_valid(12998996, 12547600, 14058984, 14058984, 0) ||
        sample_values_valid(12998996, 12547600, 14058983, 14058984, 0) ||
        sample_values_valid(12547600, 12547600, 14058985, 14058984, 0) ||
        sample_values_valid(12998996, 12547600, UINT32_MAX, 14058984, 1)) {
        puts("perf-group-sample FAILED: sample value validation contract");
        return 1;
    }
    if (check_partial_period(1) || check_partial_period(0)) {
        puts("perf-group-sample FAILED: partial sampling period lost");
        return 1;
    }
#endif
    if (check_group_sample(0) || check_group_sample(1)) return 1;
    puts("STARRY_PERF_GROUP_SAMPLE_OK");
    return 0;
}
