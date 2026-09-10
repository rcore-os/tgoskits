/* Deterministic finite-duration PERF_RECORD_LOST regression. */
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
#include <sys/wait.h>
#include <unistd.h>

#define PERF_TYPE_RAW 4u
#define PERF_SAMPLE_IP (1ull << 0)
#define PERF_SAMPLE_ID_FIELDS ((1ull << 1) | (1ull << 2) | (1ull << 6) | \
                              (1ull << 7) | (1ull << 9) | (1ull << 16))
#define PERF_ATTR_SAMPLE_ID_ALL (1ull << 18)
#define PERF_EVENT_IOC_ID 0x80082407u
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

#if defined(__aarch64__)
static volatile uint64_t sink;

static void burn(uint64_t iterations) {
    for (uint64_t i = 0; i < iterations; i++) {
        sink += i * 3u + 1u;
    }
}

/* Stop producing as soon as the required ring transition is observable. A
 * fixed instruction budget depends on the emulator's execution speed and can
 * exhaust the suite deadline long after the ring has already overflowed. */
static int produce_until(int fd, struct perf_event_mmap_page *meta,
                         uint64_t previous_head, int require_loss) {
    struct timespec start, now;
    if (clock_gettime(CLOCK_MONOTONIC, &start)) return 1;
    for (;;) {
        burn(require_loss ? 10000 : 1);
        uint64_t values[2];
        if (read(fd, values, sizeof(values)) != sizeof(values)) return 1;
        if (require_loss ? values[1] != 0 :
            __atomic_load_n(&meta->data_head, __ATOMIC_ACQUIRE) > previous_head)
            return 0;
        if (clock_gettime(CLOCK_MONOTONIC, &now) ||
            now.tv_sec - start.tv_sec >= 10) {
            printf("perf-hw-lost FAILED: deadline waiting for %s\n",
                   require_loss ? "ring loss" : "loss record");
            return 1;
        }
    }
}

static void ring_copy(const uint8_t *ring, uint64_t size, uint64_t at,
                      void *dst, size_t len) {
    for (size_t i = 0; i < len; i++) {
        ((uint8_t *)dst)[i] = ring[(at + i) % size];
    }
}

static uint64_t count_lost(const uint8_t *ring, uint64_t size, uint64_t tail,
                           uint64_t head, uint64_t *records, int trailer,
                           uint64_t id, uint64_t *invalid) {
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
            uint64_t words[9] = {0};
            unsigned expected = trailer ? sizeof(words) : 24;
            if (header.size != expected) {
                printf("LOST size=%u expected=%u\n", header.size, expected);
                (*invalid)++;
            } else {
                ring_copy(ring, size, tail, words, expected);
                if (words[1] != id) (*invalid)++;
                if (trailer &&
                    (words[4] == 0 || words[5] != id || words[6] != id ||
                     words[7] != 0 || words[8] != id))
                    (*invalid)++;
                if (trailer) {
                    /* The emptied ring fits LOST plus its following sample.
                     * Both must carry the same emission identity and time,
                     * including system-wide IRQs outside the test task. */
                    uint64_t sample[8] = {0};
                    if (tail + header.size + sizeof(sample) > head) {
                        (*invalid)++;
                    } else {
                        ring_copy(ring, size, tail + header.size, sample, sizeof(sample));
                        if ((uint32_t)sample[0] != 9 || sample[1] != id)
                            (*invalid)++;
                        for (unsigned i = 3; i <= 7; ++i)
                            if (words[i] != sample[i]) (*invalid)++;
                    }
                }
            }
        }
        tail += header.size;
    }
    return total;
}
#endif

#if defined(__aarch64__)
static int check_lost(int trailer, int system_wide) {
    struct perf_event_attr_v0 attr = {
        .type = PERF_TYPE_RAW,
        .size = sizeof(attr),
        .config = ARM_PMU_EVT_CPU_CYCLES,
        .sample_period = SAMPLE_PERIOD,
        .sample_type = PERF_SAMPLE_IP | PERF_SAMPLE_ID_FIELDS,
        .read_format = PERF_FORMAT_LOST,
        .flags = PERF_ATTR_FLAG_DISABLED | (trailer ? PERF_ATTR_SAMPLE_ID_ALL : 0),
    };
    int fd = (int)syscall(SYS_PERF_EVENT_OPEN, &attr,
                          system_wide ? -1 : 0, system_wide ? 0 : -1, -1, 0ul);
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
    uint64_t id;
    if (syscall(SYS_ioctl, fd, PERF_EVENT_IOC_ID, &id)) return 1;

    if (ioctl(fd, PERF_EVENT_IOC_RESET, 0) ||
        ioctl(fd, PERF_EVENT_IOC_ENABLE, 0) ||
        produce_until(fd, meta, 0, 1) ||
        ioctl(fd, PERF_EVENT_IOC_DISABLE, 0)) return 1;

    uint64_t pending[2];
    if (read(fd, pending, sizeof(pending)) != sizeof(pending) || pending[1] == 0)
        return 1;
    uint64_t first_head = __atomic_load_n(&meta->data_head, __ATOMIC_ACQUIRE);
    __atomic_store_n(&meta->data_tail, first_head, __ATOMIC_RELEASE);

    /* The next overflow must flush pending loss before its sample. */
    if (ioctl(fd, PERF_EVENT_IOC_ENABLE, 0) ||
        produce_until(fd, meta, first_head, 0) ||
        ioctl(fd, PERF_EVENT_IOC_DISABLE, 0)) return 1;
    uint64_t second_head = __atomic_load_n(&meta->data_head, __ATOMIC_ACQUIRE);

    uint64_t records = 0, invalid = 0;
    uint64_t in_band = count_lost(ring, meta->data_size, first_head,
                                  second_head, &records, trailer, id, &invalid);
    uint64_t read_values[2] = {0, 0};
    ssize_t read_size = read(fd, read_values, sizeof(read_values));

    printf("STARRY_PERF_LOST records=%llu in_band=%llu read_total=%llu "
           "first_head=%llu second_head=%llu pending=%llu invalid=%llu trailer=%d system=%d\n",
           (unsigned long long)records, (unsigned long long)in_band,
           (unsigned long long)read_values[1],
           (unsigned long long)first_head, (unsigned long long)second_head,
           (unsigned long long)pending[1], (unsigned long long)invalid,
           trailer, system_wide);

    munmap(mapping, RING_BYTES);
    close(fd);
    if (invalid != 0 || records == 0 || in_band == 0 || read_size != 16 ||
        in_band != pending[1] || read_values[1] < in_band) {
        puts("perf-hw-lost FAILED: missing or inconsistent loss accounting");
        return 1;
    }
    return 0;
}

/* The parent is ineligible on CPU1. Only its CPU0 child can fill the shared
 * ring, and pipe gates keep that child alive while the root fd is inspected. */
static int check_inherited_lost(void) {
    cpu_set_t cpus;
    CPU_ZERO(&cpus);
    CPU_SET(1, &cpus);
    if (sched_setaffinity(0, sizeof(cpus), &cpus)) return 1;
    struct perf_event_attr_v0 attr = {
        .type = PERF_TYPE_RAW, .size = sizeof(attr),
        .config = ARM_PMU_EVT_CPU_CYCLES, .sample_period = SAMPLE_PERIOD,
        .sample_type = PERF_SAMPLE_IP | PERF_SAMPLE_ID_FIELDS,
        .read_format = PERF_FORMAT_LOST,
        .flags = (1ull << 1) | PERF_ATTR_SAMPLE_ID_ALL,
    };
    int fd = (int)syscall(SYS_PERF_EVENT_OPEN, &attr, 0, 0, -1, 0ul);
    if (fd < 0) return 1;
    struct perf_event_mmap_page *meta = mmap(NULL, RING_BYTES,
        PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (meta == MAP_FAILED) return 1;
    uint64_t id;
    int ready[2], go[2];
    if (ioctl(fd, PERF_EVENT_IOC_ID, &id) || pipe(ready) || pipe(go)) return 1;
    pid_t child = fork();
    if (child < 0) return 1;
    if (!child) {
        close(ready[0]);
        close(go[1]);
        CPU_ZERO(&cpus);
        CPU_SET(0, &cpus);
        if (sched_setaffinity(0, sizeof(cpus), &cpus)) _exit(1);
        struct timespec start, now;
        if (clock_gettime(CLOCK_MONOTONIC, &start)) _exit(1);
        while (__atomic_load_n(&meta->data_head, __ATOMIC_ACQUIRE) <
               meta->data_size - 64) {
            burn(10000);
            if (clock_gettime(CLOCK_MONOTONIC, &now) ||
                now.tv_sec - start.tv_sec >= 10) _exit(1);
        }
        /* More overflows after a full ring must be charged to the parent. */
        burn(100000);
        char token = 0;
        if (write(ready[1], &token, 1) != 1 || read(go[0], &token, 1) != 1)
            _exit(1);
        uint64_t head = __atomic_load_n(&meta->data_head, __ATOMIC_ACQUIRE);
        if (produce_until(fd, meta, head, 0) ||
            write(ready[1], &token, 1) != 1 || read(go[0], &token, 1) != 1)
            _exit(1);
        _exit(0);
    }
    close(ready[1]);
    close(go[0]);
    char token = 0;
    uint64_t pending[2] = {0};
    if (read(ready[0], &token, 1) != 1 ||
        read(fd, pending, sizeof(pending)) != sizeof(pending)) return 1;
    uint64_t first = __atomic_load_n(&meta->data_head, __ATOMIC_ACQUIRE);
    __atomic_store_n(&meta->data_tail, first, __ATOMIC_RELEASE);
    if (write(go[1], &token, 1) != 1 || read(ready[0], &token, 1) != 1 ||
        ioctl(fd, PERF_EVENT_IOC_DISABLE, 0)) return 1;
    uint64_t head = __atomic_load_n(&meta->data_head, __ATOMIC_ACQUIRE);
    const uint8_t *ring = (const uint8_t *)meta + meta->data_offset;
    uint64_t lost[9] = {0};
    ring_copy(ring, meta->data_size, first, lost, sizeof(lost));
    uint64_t sample[8] = {0};
    ring_copy(ring, meta->data_size, first + sizeof(lost), sample, sizeof(sample));
    uint64_t total[2] = {0};
    int failed = read(fd, total, sizeof(total)) != sizeof(total) ||
        pending[1] == 0 || head - first < sizeof(lost) + sizeof(sample) ||
        (uint32_t)lost[0] != PERF_RECORD_LOST || (lost[0] >> 48) != sizeof(lost) ||
        lost[1] != id || lost[2] == 0 || lost[2] > total[1] ||
        lost[5] != id || lost[6] != id || lost[8] != id ||
        (uint32_t)sample[0] != 9 || (sample[0] >> 48) != sizeof(sample) ||
        sample[1] != id || sample[5] != id || sample[6] == 0 || sample[6] == id;
    printf("INHERITED_LOST pending=%llu total=%llu lost=%llu id=%llu "
           "lost_stream=%llu sample_stream=%llu failed=%d\n",
           (unsigned long long)pending[1], (unsigned long long)total[1],
           (unsigned long long)lost[2], (unsigned long long)id,
           (unsigned long long)lost[6], (unsigned long long)sample[6], failed);
    int status;
    if (write(go[1], &token, 1) != 1 || waitpid(child, &status, 0) != child ||
        !WIFEXITED(status) || WEXITSTATUS(status)) failed = 1;
    close(ready[0]);
    close(go[1]);
    munmap(meta, RING_BYTES);
    close(fd);
    return failed;
}
#endif

int main(void) {
#if defined(__aarch64__)
    cpu_set_t cpus;
    CPU_ZERO(&cpus);
    CPU_SET(0, &cpus);
    if (sched_setaffinity(0, sizeof(cpus), &cpus)) return 1;
    if (check_lost(0, 0) || check_lost(1, 0) || check_lost(1, 1) ||
        check_inherited_lost()) return 1;
#endif
    puts("STARRY_PERF_LOST_OK");
    return 0;
}
