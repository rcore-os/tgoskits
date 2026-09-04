/* Linux v7.1 PERF_SAMPLE_READ layout and monotonic-value regression. */
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
#define PERF_ATTR_FLAG_DISABLED (1ull << 0)
#define PERF_EVENT_IOC_ENABLE 0x2400u
#define PERF_EVENT_IOC_DISABLE 0x2401u
#define PERF_EVENT_IOC_RESET 0x2403u
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

static void ring_copy(const uint8_t *ring, uint64_t size, uint64_t at,
                      void *dst, size_t len) {
    for (size_t i = 0; i < len; i++) {
        ((uint8_t *)dst)[i] = ring[(at + i) % size];
    }
}
#endif

int main(void) {
#if !defined(__aarch64__)
    puts("STARRY_PERF_SAMPLE_READ_OK");
    return 0;
#else
    struct perf_event_attr_v0 attr = {
        .type = PERF_TYPE_RAW,
        .size = sizeof(attr),
        .config = 0x11,
        .sample_period = 100000,
        .sample_type = PERF_SAMPLE_IP | PERF_SAMPLE_READ,
        .flags = PERF_ATTR_FLAG_DISABLED,
    };
    int fd = (int)syscall(SYS_PERF_EVENT_OPEN, &attr, 0, -1, -1, 0ul);
    if (fd < 0) {
        printf("perf-sample-read FAILED: open errno=%d\n", errno);
        return 1;
    }
    void *mapping = mmap(NULL, RING_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED,
                         fd, 0);
    if (mapping == MAP_FAILED) {
        printf("perf-sample-read FAILED: mmap errno=%d\n", errno);
        close(fd);
        return 1;
    }
    struct perf_event_mmap_page *meta = mapping;
    ioctl(fd, PERF_EVENT_IOC_RESET, 0);
    ioctl(fd, PERF_EVENT_IOC_ENABLE, 0);
    for (uint64_t i = 0; i < 16000000; i++) {
        sink += (i ^ (sink << 1)) + 1;
    }
    ioctl(fd, PERF_EVENT_IOC_DISABLE, 0);

    uint64_t head = meta->data_head;
    __sync_synchronize();
    uint64_t tail = meta->data_tail;
    const uint8_t *ring = (const uint8_t *)mapping + meta->data_offset;
    uint64_t first = 0, last = 0, samples = 0;
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
            /* Linux order: header, PERF_SAMPLE_IP, PERF_SAMPLE_READ value. */
            uint64_t value = 0;
            if (header.size < 24) {
                corrupt = 1;
                break;
            }
            ring_copy(ring, meta->data_size, start + 16, &value, sizeof(value));
            if (samples == 0) {
                first = value;
            } else if (value <= last) {
                corrupt = 1;
                break;
            }
            last = value;
            samples++;
        }
        tail += header.size;
    }
    meta->data_tail = tail;
    __sync_synchronize();
    printf("STARRY_PERF_SAMPLE_READ samples=%llu first=%llu last=%llu corrupt=%d\n",
           (unsigned long long)samples, (unsigned long long)first,
           (unsigned long long)last, corrupt);
    munmap(mapping, RING_BYTES);
    close(fd);
    if (corrupt || samples < 2 || last <= first) {
        puts("perf-sample-read FAILED: non-monotonic sample read values");
        return 1;
    }
    puts("STARRY_PERF_SAMPLE_READ_OK");
    return 0;
#endif
}
