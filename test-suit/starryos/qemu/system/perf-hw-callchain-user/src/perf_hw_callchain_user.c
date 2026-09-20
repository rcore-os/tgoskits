/* User frame-pointer callchain regression: leaf plus three callers. */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <fcntl.h>
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
#endif

int main(void) {
#if !defined(__aarch64__)
    puts("STARRY_PERF_CALLCHAIN_USER_OK");
    return 0;
#else
    struct perf_event_attr_v3 attr = {
        .type = PERF_TYPE_RAW,
        .size = sizeof(attr),
        .config = 0x11,
        .sample_period = 100000,
        .sample_type = PERF_SAMPLE_IP | PERF_SAMPLE_TID | PERF_SAMPLE_TIME |
                       PERF_SAMPLE_ID | PERF_SAMPLE_CALLCHAIN | PERF_SAMPLE_REGS_USER,
        .sample_regs_user = 1ull << 30, /* upstream perf's AArch64 FP unwinder requests LR */
        .flags = PERF_ATTR_FLAG_DISABLED | (1ull << 5), /* exclude_kernel */
    };
    int fd = (int)syscall(SYS_PERF_EVENT_OPEN, &attr, 0, -1, -1, 0ul);
    if (fd < 0) {
        printf("perf-callchain-user FAILED: open errno=%d\n", errno);
        return 1;
    }
    void *mapping = mmap(NULL, RING_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED,
                         fd, 0);
    if (mapping == MAP_FAILED) {
        printf("perf-callchain-user FAILED: mmap errno=%d\n", errno);
        close(fd);
        return 1;
    }
    struct perf_event_mmap_page *meta = mapping;
    uint64_t event_id = 0;
    if (ioctl(fd, PERF_EVENT_IOC_ID, &event_id) != 0 || event_id == 0) {
        puts("perf-callchain-user FAILED: event id");
        return 1;
    }
    ioctl(fd, PERF_EVENT_IOC_RESET, 0);
    ioctl(fd, PERF_EVENT_IOC_ENABLE, 0);
    outer();
    ioctl(fd, PERF_EVENT_IOC_DISABLE, 0);

    uint64_t head = meta->data_head;
    __sync_synchronize();
    uint64_t tail = meta->data_tail;
    const uint8_t *ring = (const uint8_t *)mapping + meta->data_offset;
    uint64_t samples = 0, user_chains = 0, max_user_ips = 0, user_regs = 0;
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
            samples++;
            /* header, ip, pid/tid, time, id, then callchain nr. */
            uint64_t cursor = 8 + 8 + 8 + 8;
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
            if (cursor + 8 > header.size) { corrupt = 1; break; }
            ring_copy(ring, meta->data_size, start + cursor, &abi, 8);
            cursor += 8;
            if ((header.misc & 7) == 2) {
                if (abi != 2 || cursor + 8 != header.size) { corrupt = 1; break; }
                ring_copy(ring, meta->data_size, start + cursor, &lr, 8);
                if (lr == 0 || lr >= (1ull << 48) || (lr & 3)) { corrupt = 1; break; }
                user_regs++;
            } else if (abi != 0 || cursor != header.size) { corrupt = 1; break; }
        }
        tail += header.size;
    }

    printf("STARRY_PERF_CALLCHAIN_USER samples=%llu user_chains=%llu "
           "max_user_ips=%llu corrupt=%d\n",
           (unsigned long long)samples, (unsigned long long)user_chains,
           (unsigned long long)max_user_ips, corrupt);
    munmap(mapping, RING_BYTES);
    close(fd);
    printf("STARRY_PERF_USER_LR samples=%llu\n", (unsigned long long)user_regs);
    if (corrupt || samples == 0 || user_chains == 0 || max_user_ips < 4 || user_regs == 0) {
        puts("perf-callchain-user FAILED: no user callchain with four IPs");
        return 1;
    }
    puts("STARRY_PERF_CALLCHAIN_USER_OK");
    return 0;
#endif
}
