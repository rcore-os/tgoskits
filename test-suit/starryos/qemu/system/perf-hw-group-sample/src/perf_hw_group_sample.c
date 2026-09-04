/* Linux v7.1 leader-first PERF_SAMPLE_READ group regression. */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
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

static volatile uint64_t sink;

static void ring_copy(const uint8_t *ring, uint64_t size, uint64_t at,
                      void *dst, size_t len) {
    for (size_t i = 0; i < len; i++) {
        ((uint8_t *)dst)[i] = ring[(at + i) % size];
    }
}

int main(void) {
#if !defined(__aarch64__)
    puts("STARRY_PERF_GROUP_SAMPLE_OK");
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
    ioctl(leader, PERF_EVENT_IOC_RESET, 0);
    ioctl(leader, PERF_EVENT_IOC_ENABLE, 0);
    for (uint64_t i = 0; i < 16000000; i++) {
        sink += (i ^ (sink >> 1)) + 3;
    }
    ioctl(leader, PERF_EVENT_IOC_DISABLE, 0);

    uint64_t head = meta->data_head;
    __sync_synchronize();
    uint64_t tail = meta->data_tail;
    const uint8_t *ring = (const uint8_t *)mapping + meta->data_offset;
    uint64_t samples = 0, last_leader = 0, last_member = 0;
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
                fields[4] != member_id || fields[1] <= last_leader ||
                fields[3] < last_member) {
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
            last_leader = fields[1];
            last_member = fields[3];
            samples++;
        }
        tail += header.size;
    }
    printf("STARRY_PERF_GROUP_SAMPLE samples=%llu leader=%llu member=%llu corrupt=%d\n",
           (unsigned long long)samples, (unsigned long long)last_leader,
           (unsigned long long)last_member, corrupt);
    munmap(mapping, RING_BYTES);
    close(member);
    close(leader);
    if (corrupt || samples < 2 || last_leader == 0 || last_member == 0) {
        puts("perf-group-sample FAILED: malformed or empty group snapshot");
        return 1;
    }
    puts("STARRY_PERF_GROUP_SAMPLE_OK");
    return 0;
#endif
}
