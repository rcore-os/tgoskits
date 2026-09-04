/* Deterministic finite-duration PERF_RECORD_LOST regression. */
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
#define PERF_FORMAT_LOST (1ull << 4)
#define PERF_ATTR_FLAG_DISABLED (1ull << 0)
#define PERF_EVENT_IOC_ENABLE 0x2400u
#define PERF_EVENT_IOC_DISABLE 0x2401u
#define PERF_EVENT_IOC_RESET 0x2403u
#define PERF_RECORD_LOST 2u
#define ARM_PMU_EVT_CPU_CYCLES 0x11ull
#define SYS_PERF_EVENT_OPEN 241
#define PAGE_SIZE_4K 4096u
#define RING_BYTES (2u * PAGE_SIZE_4K)
#define SAMPLE_PERIOD 100000ull

struct perf_event_attr_v0 {
    uint32_t type;
    uint32_t size;
    uint64_t config;
    uint64_t sample_period;
    uint64_t sample_type;
    uint64_t read_format;
    uint64_t flags;
    uint32_t wakeup_events;
    uint32_t bp_type;
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
    uint16_t misc;
    uint16_t size;
};

_Static_assert(sizeof(struct perf_event_attr_v0) == 64, "perf attr v0 size");
_Static_assert(offsetof(struct perf_event_mmap_page, data_head) == 1024,
               "perf data_head offset");

static volatile uint64_t sink;

static void burn(uint64_t iterations) {
    for (uint64_t i = 0; i < iterations; i++) {
        sink += i * 3u + 1u;
    }
}

static void ring_copy(const uint8_t *ring, uint64_t size, uint64_t at,
                      void *dst, size_t len) {
    for (size_t i = 0; i < len; i++) {
        ((uint8_t *)dst)[i] = ring[(at + i) % size];
    }
}

static uint64_t count_lost(const uint8_t *ring, uint64_t size, uint64_t tail,
                           uint64_t head, uint64_t *records) {
    uint64_t total = 0;
    while (tail < head) {
        struct perf_event_header header;
        ring_copy(ring, size, tail % size, &header, sizeof(header));
        if (header.size < sizeof(header) || tail + header.size > head) {
            break;
        }
        if (header.type == PERF_RECORD_LOST && header.size >= 24) {
            uint64_t lost;
            ring_copy(ring, size, tail % size + 16, &lost, sizeof(lost));
            total += lost;
            (*records)++;
        }
        tail += header.size;
    }
    return total;
}

int main(void) {
#if !defined(__aarch64__)
    puts("STARRY_PERF_LOST_OK");
    return 0;
#else
    struct perf_event_attr_v0 attr = {
        .type = PERF_TYPE_RAW,
        .size = sizeof(attr),
        .config = ARM_PMU_EVT_CPU_CYCLES,
        .sample_period = SAMPLE_PERIOD,
        .sample_type = PERF_SAMPLE_IP,
        .read_format = PERF_FORMAT_LOST,
        .flags = PERF_ATTR_FLAG_DISABLED,
    };
    int fd = (int)syscall(SYS_PERF_EVENT_OPEN, &attr, 0, -1, -1, 0ul);
    if (fd < 0) {
        printf("perf-hw-lost FAILED: open errno=%d\n", errno);
        return 1;
    }
    void *mapping = mmap(NULL, RING_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED,
                         fd, 0);
    if (mapping == MAP_FAILED) {
        printf("perf-hw-lost FAILED: mmap errno=%d\n", errno);
        close(fd);
        return 1;
    }
    struct perf_event_mmap_page *meta = mapping;
    const uint8_t *ring = (const uint8_t *)mapping + meta->data_offset;

    ioctl(fd, PERF_EVENT_IOC_RESET, 0);
    ioctl(fd, PERF_EVENT_IOC_ENABLE, 0);
    burn(80000000ull);

    uint64_t first_head = meta->data_head;
    __sync_synchronize();
    meta->data_tail = first_head;
    __sync_synchronize();

    /* The next overflow must flush pending loss before its sample. */
    burn(10000000ull);
    ioctl(fd, PERF_EVENT_IOC_DISABLE, 0);
    uint64_t second_head = meta->data_head;
    __sync_synchronize();

    uint64_t records = 0;
    uint64_t in_band = count_lost(ring, meta->data_size, first_head,
                                  second_head, &records);
    uint64_t read_values[2] = {0, 0};
    ssize_t read_size = read(fd, read_values, sizeof(read_values));

    printf("STARRY_PERF_LOST records=%llu in_band=%llu read_total=%llu "
           "first_head=%llu second_head=%llu\n",
           (unsigned long long)records, (unsigned long long)in_band,
           (unsigned long long)read_values[1],
           (unsigned long long)first_head, (unsigned long long)second_head);

    munmap(mapping, RING_BYTES);
    close(fd);
    if (records == 0 || in_band == 0 || read_size != 16 ||
        read_values[1] < in_band) {
        puts("perf-hw-lost FAILED: missing or inconsistent loss accounting");
        return 1;
    }
    puts("STARRY_PERF_LOST_OK");
    return 0;
#endif
}
