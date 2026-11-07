/*
 * bpf_perf_event_output against perf files that user space closes.
 *
 * A raw tracepoint program on sys_enter calls the helper on every syscall and
 * records its return value. The helper must deliver to BPF-output events,
 * reject other or closed fds, and never be the one that releases a perf
 * event: releasing a tracepoint event must sleep, while the helper runs with
 * local IRQs disabled. Delivery to a BPF-output event is checked against a
 * mapped ring: without a mapping the helper would drop the record while
 * still returning 0, which would prove nothing. One thread per CPU keeps
 * the helper running while the
 * main thread repeatedly publishes and closes tracepoint events; a run
 * stopped by the time limit before CLOSE_MIN_ROUNDS rounds fails, because it
 * has not covered the round range where the unfixed kernel was seen to
 * panic.
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#ifndef SYS_bpf
#error "SYS_bpf is required"
#endif
#ifndef SYS_perf_event_open
#error "SYS_perf_event_open is required"
#endif

#define BPF_MAP_CREATE 0
#define BPF_MAP_LOOKUP_ELEM 1
#define BPF_MAP_UPDATE_ELEM 2
#define BPF_PROG_LOAD 5
#define BPF_RAW_TRACEPOINT_OPEN 17
#define BPF_MAP_TYPE_ARRAY 2
#define BPF_MAP_TYPE_PERF_EVENT_ARRAY 4
/* BPF_RAW_TRACEPOINT_OPEN requires a raw-tracepoint program in Linux. */
#define BPF_PROG_TYPE_RAW_TRACEPOINT 17
#define BPF_PSEUDO_MAP_FD 1

#define BPF_LD 0x00
#define BPF_ST 0x02
#define BPF_STX 0x03
#define BPF_JMP 0x05
#define BPF_ALU 0x04
#define BPF_ALU64 0x07
#define BPF_W 0x00
#define BPF_DW 0x18
#define BPF_IMM 0x00
#define BPF_MEM 0x60
#define BPF_K 0x00
#define BPF_X 0x08
#define BPF_ADD 0x00
#define BPF_MOV 0xb0
#define BPF_JEQ 0x10
#define BPF_CALL 0x80
#define BPF_EXIT 0x90

#define HELPER_MAP_LOOKUP_ELEM 1
#define HELPER_PERF_EVENT_OUTPUT 25
#define BPF_F_CURRENT_CPU 0xffffffffu

#define PERF_TYPE_SOFTWARE 1
#define PERF_TYPE_TRACEPOINT 2
#define PERF_COUNT_SW_BPF_OUTPUT 10
#define PERF_SAMPLE_RAW (1u << 10)
#ifndef PERF_RECORD_SAMPLE
#define PERF_RECORD_SAMPLE 9u
#endif
#ifndef PERF_EVENT_IOC_ENABLE
#define PERF_EVENT_IOC_ENABLE 0x2400u
#endif
/* Linux perf maps one metadata page plus a power-of-two number of data
 * pages on a perf event fd. */
#define PERF_MMAP_DATA_PAGES 8u
#define PERF_MMAP_TOTAL_BYTES ((size_t)(1u + PERF_MMAP_DATA_PAGES) * 4096u)

/* Rounds of publish-and-close; the time limit bounds slow emulation. */
#define CLOSE_ROUNDS 2048
/*
 * Rounds that a run stopped by the time limit must still have completed.
 * The unfixed kernel panicked within the first 64 rounds in all eight local
 * runs (QEMU TCG, -smp 4; progress logged every 64 rounds), so 1024 keeps
 * a 16x margin over that bound. A run below this count has not covered the
 * known trigger range and fails instead of reporting success.
 */
#define CLOSE_MIN_ROUNDS 1024
/*
 * Whole-loop budget in nanoseconds (20 s). The full 2048 rounds finish in
 * well under a second on the reference QEMU runner, so this only bounds
 * badly slowed emulation; nanoseconds (not whole seconds) so that local
 * checks can shrink the budget below one second to exercise the gate,
 * which whole seconds cannot reach.
 */
#define CLOSE_TIME_NS 20000000000L

struct bpf_insn {
    uint8_t code;
    uint8_t regs;
    int16_t off;
    int32_t imm;
};

struct perf_event_attr {
    uint32_t type;
    uint32_t size;
    uint64_t config;
    uint64_t sample_period;
    uint64_t sample_type;
    uint64_t read_format;
    uint64_t flags;
    uint32_t wakeup_events;
    uint32_t bp_type;
    uint64_t config1;
    uint64_t config2;
};

struct perf_event_header {
    uint32_t type;
    uint16_t misc;
    uint16_t size;
};

/* First page of the mmap ring; field order matches Linux perf_event.h so
 * the data_head / data_tail protocol is the shared one. */
struct perf_event_mmap_page {
    uint32_t version;
    uint32_t compat_version;
    uint32_t lock;
    uint32_t index;
    int64_t offset;
    uint64_t time_enabled;
    uint64_t time_running;
    uint64_t capabilities;
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
};

static long sys_bpf(int cmd, void *attr, unsigned int size) {
    return syscall(SYS_bpf, cmd, attr, size);
}

static long elapsed_ns(const struct timespec *start) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (now.tv_sec - start->tv_sec) * 1000000000L + now.tv_nsec - start->tv_nsec;
}

static struct bpf_insn insn(uint8_t code, uint8_t dst, uint8_t src,
                            int16_t off, int32_t imm) {
    struct bpf_insn i = {
        .code = code,
        .regs = (uint8_t)((dst & 0xf) | ((src & 0xf) << 4)),
        .off = off,
        .imm = imm,
    };
    return i;
}

static int map_create(uint32_t type, uint32_t value_size, uint32_t entries) {
    struct {
        uint32_t map_type;
        uint32_t key_size;
        uint32_t value_size;
        uint32_t max_entries;
        uint32_t map_flags;
    } attr = {
        .map_type = type,
        .key_size = sizeof(uint32_t),
        .value_size = value_size,
        .max_entries = entries,
    };
    return (int)sys_bpf(BPF_MAP_CREATE, &attr, sizeof(attr));
}

static int map_elem(int cmd, int map_fd, uint32_t key, void *value) {
    struct {
        uint32_t map_fd;
        uint32_t pad;
        uint64_t key;
        uint64_t value;
        uint64_t flags;
    } attr = {
        .map_fd = (uint32_t)map_fd,
        .key = (uint64_t)(uintptr_t)&key,
        .value = (uint64_t)(uintptr_t)value,
    };
    return (int)sys_bpf(cmd, &attr, sizeof(attr));
}

/* ret = bpf_perf_event_output(ctx, perf_map, CURRENT_CPU, &zero, 8); *result = ret */
static int load_program(int perf_map, int result_map) {
    struct bpf_insn prog[] = {
        insn(BPF_LD | BPF_DW | BPF_IMM, 2, BPF_PSEUDO_MAP_FD, 0, perf_map),
        insn(0, 0, 0, 0, 0),
        /* A 32-bit move zero-extends its immediate, so r3 holds
         * 0x00000000ffffffff, the 64-bit BPF_F_CURRENT_CPU value Linux
         * defines and its helper accepts; a 64-bit move would sign-extend
         * the -1 immediate to 0xffffffffffffffff, which Linux rejects. */
        insn(BPF_ALU | BPF_MOV | BPF_K, 3, 0, 0, (int32_t)BPF_F_CURRENT_CPU),
        insn(BPF_ALU64 | BPF_MOV | BPF_X, 4, 10, 0, 0),
        insn(BPF_ALU64 | BPF_ADD | BPF_K, 4, 0, 0, -8),
        insn(BPF_ST | BPF_MEM | BPF_DW, 10, 0, -8, 0),
        insn(BPF_ALU64 | BPF_MOV | BPF_K, 5, 0, 0, 8),
        insn(BPF_JMP | BPF_CALL, 0, 0, 0, HELPER_PERF_EVENT_OUTPUT),
        insn(BPF_ALU64 | BPF_MOV | BPF_X, 6, 0, 0, 0),
        insn(BPF_LD | BPF_DW | BPF_IMM, 1, BPF_PSEUDO_MAP_FD, 0, result_map),
        insn(0, 0, 0, 0, 0),
        insn(BPF_ALU64 | BPF_MOV | BPF_X, 2, 10, 0, 0),
        insn(BPF_ALU64 | BPF_ADD | BPF_K, 2, 0, 0, -12),
        insn(BPF_ST | BPF_MEM | BPF_W, 10, 0, -12, 0),
        insn(BPF_JMP | BPF_CALL, 0, 0, 0, HELPER_MAP_LOOKUP_ELEM),
        insn(BPF_JMP | BPF_JEQ | BPF_K, 0, 0, 1, 0),
        insn(BPF_STX | BPF_MEM | BPF_DW, 0, 6, 0, 0),
        insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 0),
        insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    static const char license[] = "GPL";
    struct {
        uint32_t prog_type;
        uint32_t insn_cnt;
        uint64_t insns;
        uint64_t license;
        uint32_t log_level;
        uint32_t log_size;
        uint64_t log_buf;
        uint32_t kern_version;
        uint32_t prog_flags;
    } attr = {
        .prog_type = BPF_PROG_TYPE_RAW_TRACEPOINT,
        .insn_cnt = sizeof(prog) / sizeof(prog[0]),
        .insns = (uint64_t)(uintptr_t)prog,
        .license = (uint64_t)(uintptr_t)license,
    };
    return (int)sys_bpf(BPF_PROG_LOAD, &attr, sizeof(attr));
}

static int attach_sys_enter(int prog_fd) {
    static const char name[] = "sys_enter";
    struct {
        uint64_t name;
        uint32_t prog_fd;
        uint32_t pad;
    } attr = {
        .name = (uint64_t)(uintptr_t)name,
        .prog_fd = (uint32_t)prog_fd,
    };
    return (int)sys_bpf(BPF_RAW_TRACEPOINT_OPEN, &attr, sizeof(attr));
}

static int open_event(uint32_t type, uint64_t config) {
    struct perf_event_attr attr;
    memset(&attr, 0, sizeof(attr));
    attr.type = type;
    attr.size = sizeof(attr);
    attr.config = config;
    if (type == PERF_TYPE_SOFTWARE) {
        attr.sample_type = PERF_SAMPLE_RAW;
        attr.wakeup_events = 1;
    }
    return (int)syscall(SYS_perf_event_open, &attr, 0, -1, -1, 0ul);
}

static long read_tracepoint_id(void) {
    static const char path[] = "/sys/kernel/debug/tracing/events/raw_syscalls/sys_enter/id";
    char buf[32];
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        printf("open %s failed errno=%d\n", path, errno);
        return -1;
    }
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    close(fd);
    if (n <= 0) {
        printf("read %s failed errno=%d\n", path, errno);
        return -1;
    }
    buf[n] = '\0';
    return strtol(buf, NULL, 10);
}

static int perf_map;
static int result_map;
static int ncpu;

static int publish(int fd) {
    uint32_t value = (uint32_t)fd;
    for (int cpu = 0; cpu < ncpu; cpu++) {
        if (map_elem(BPF_MAP_UPDATE_ELEM, perf_map, (uint32_t)cpu, &value) != 0) {
            printf("perf map update failed errno=%d\n", errno);
            return -1;
        }
    }
    return 0;
}

/* Run the program on this thread's next syscall and return the helper result. */
static int64_t helper_result(void) {
    uint64_t value = 0x5a5a5a5a5a5a5a5aull;
    if (map_elem(BPF_MAP_UPDATE_ELEM, result_map, 0, &value) != 0 ||
        map_elem(BPF_MAP_LOOKUP_ELEM, result_map, 0, &value) != 0) {
        printf("result map access failed errno=%d\n", errno);
        return INT64_MIN;
    }
    return (int64_t)value;
}

/* Read by the caller threads, set by the main thread once the race rounds
 * end. Atomic so the cross-thread access is a defined load/store pair. */
static _Atomic int stop_callers;

static void *syscall_caller(void *arg) {
    (void)arg;
    while (!atomic_load_explicit(&stop_callers, memory_order_relaxed)) {
        syscall(SYS_getpid);
    }
    return NULL;
}

static int check_targets(uint32_t tp_id) {
    int failures = 0;

    /* The 8 bytes load_program() stores at the helper's data pointer. */
    static const uint8_t written_payload[8] = {0};

    int out_fd = open_event(PERF_TYPE_SOFTWARE, PERF_COUNT_SW_BPF_OUTPUT);
    if (out_fd < 0 || publish(out_fd) != 0) {
        printf("open BPF output event failed errno=%d\n", errno);
        return 1;
    }
    /* Map a ring so the helper must deliver: with no mapping it drops the
     * record and still returns 0, so ret alone proves nothing. */
    void *ring = mmap(NULL, PERF_MMAP_TOTAL_BYTES, PROT_READ | PROT_WRITE,
                      MAP_SHARED, out_fd, 0);
    if (ring == MAP_FAILED) {
        printf("mmap BPF output ring failed errno=%d\n", errno);
        close(out_fd);
        return 1;
    }
    struct perf_event_mmap_page *meta =
        (struct perf_event_mmap_page *)ring;
    /* The kernel writes records only while the event is enabled. */
    if (ioctl(out_fd, PERF_EVENT_IOC_ENABLE, 0) != 0) {
        printf("enable BPF output event failed errno=%d\n", errno);
        failures++;
    }
    int64_t ret = helper_result();
    if (ret != 0) {
        printf("BPF output target: ret=%lld, expected 0\n", (long long)ret);
        failures++;
    }
    /* The ring must hold at least one PERF_RECORD_SAMPLE whose raw payload
     * is the bytes the program passed to the helper. Both the header and
     * the u32 raw-size prefix preceding the payload are the layout Linux
     * uses for a sample with PERF_SAMPLE_RAW set. */
    uint64_t head = __atomic_load_n(&meta->data_head, __ATOMIC_ACQUIRE);
    uint64_t tail = meta->data_tail;
    uint64_t data_size = meta->data_size;
    const uint8_t *data = (const uint8_t *)ring + meta->data_offset;
    uint64_t samples = 0;
    uint64_t off = tail;
    while (off < head && data_size != 0) {
        uint64_t rel = off % data_size;
        struct perf_event_header hdr;
        for (size_t b = 0; b < sizeof(hdr); b++) {
            ((uint8_t *)&hdr)[b] = data[(rel + b) % data_size];
        }
        if (hdr.size == 0 || off + hdr.size > head) {
            printf("ring record at %llu overruns data_head\n",
                   (unsigned long long)off);
            failures++;
            break;
        }
        if (hdr.type == PERF_RECORD_SAMPLE) {
            uint32_t raw_size = 0;
            for (size_t b = 0; b < sizeof(raw_size); b++) {
                ((uint8_t *)&raw_size)[b] =
                    data[(rel + sizeof(hdr) + b) % data_size];
            }
            int mismatch = 0;
            for (uint32_t b = 0; b < raw_size; b++) {
                uint8_t byte =
                    data[(rel + sizeof(hdr) + sizeof(raw_size) + b) %
                         data_size];
                if (b >= sizeof(written_payload) ||
                    byte != written_payload[b]) {
                    mismatch = 1;
                    break;
                }
            }
            if (raw_size != sizeof(written_payload) || mismatch) {
                printf("sample record at %llu carries unexpected raw data\n",
                       (unsigned long long)off);
                failures++;
            } else {
                samples++;
            }
        }
        off += hdr.size;
    }
    if (samples == 0) {
        printf("no PERF_RECORD_SAMPLE delivered to the BPF output ring\n");
        failures++;
    }
    /* Publish the consumed position, then drop the mapping before the fd
     * is closed and its number reused: the mapping pins the ring's pages
     * and keeps the event referenced. */
    __atomic_store_n(&meta->data_tail, head, __ATOMIC_RELEASE);
    munmap(ring, PERF_MMAP_TOTAL_BYTES);

    int tp_fd = open_event(PERF_TYPE_TRACEPOINT, tp_id);
    if (tp_fd < 0 || publish(tp_fd) != 0) {
        printf("open tracepoint event failed errno=%d\n", errno);
        return 1;
    }
    ret = helper_result();
    if (ret == 0) {
        printf("tracepoint target without an output ring succeeded\n");
        failures++;
    }
    close(tp_fd);

    /* The closed fd number now names nothing; the output event is untouched. */
    ret = helper_result();
    if (ret == 0) {
        printf("closed fd succeeded\n");
        failures++;
    }
    if (publish(out_fd) != 0) {
        return 1;
    }
    ret = helper_result();
    if (ret != 0) {
        printf("BPF output target after closing another event: ret=%lld\n",
               (long long)ret);
        failures++;
    }
    close(out_fd);

    /* A new output event that reuses the fd number is the helper's target. */
    out_fd = open_event(PERF_TYPE_SOFTWARE, PERF_COUNT_SW_BPF_OUTPUT);
    if (out_fd < 0 || publish(out_fd) != 0) {
        printf("reopen BPF output event failed errno=%d\n", errno);
        return 1;
    }
    ret = helper_result();
    if (ret != 0) {
        printf("reopened BPF output target: ret=%lld\n", (long long)ret);
        failures++;
    }
    close(out_fd);
    return failures;
}

static int close_while_helpers_run(uint32_t tp_id) {
    pthread_t *threads = calloc((size_t)ncpu, sizeof(*threads));
    if (threads == NULL) {
        return 1;
    }
    for (int i = 0; i < ncpu; i++) {
        if (pthread_create(&threads[i], NULL, syscall_caller, NULL) != 0) {
            printf("pthread_create failed\n");
            return 1;
        }
    }
    int failures = 0;
    int rounds = 0;
    struct timespec start;
    clock_gettime(CLOCK_MONOTONIC, &start);
    for (; rounds < CLOSE_ROUNDS && elapsed_ns(&start) < CLOSE_TIME_NS; rounds++) {
        int fd = open_event(PERF_TYPE_TRACEPOINT, tp_id);
        if (fd < 0) {
            printf("round %d: open tracepoint event failed errno=%d\n", rounds, errno);
            failures++;
            break;
        }
        if (publish(fd) != 0) {
            failures++;
            break;
        }
        if (close(fd) != 0) {
            printf("round %d: close failed errno=%d\n", rounds, errno);
            failures++;
            break;
        }
    }
    atomic_store_explicit(&stop_callers, 1, memory_order_relaxed);
    for (int i = 0; i < ncpu; i++) {
        pthread_join(threads[i], NULL);
    }
    free(threads);
    printf("closed %d tracepoint events while the helper ran\n", rounds);
    if (elapsed_ns(&start) >= CLOSE_TIME_NS && rounds < CLOSE_MIN_ROUNDS) {
        printf("time limit reached after only %d rounds, below the minimum of %d\n", rounds,
               CLOSE_MIN_ROUNDS);
        failures++;
    }
    return failures;
}

int main(void) {
    long online = sysconf(_SC_NPROCESSORS_ONLN);
    long tp_id = read_tracepoint_id();
    if (online <= 0 || tp_id < 0) {
        return 1;
    }
    ncpu = (int)online;
    perf_map = map_create(BPF_MAP_TYPE_PERF_EVENT_ARRAY, sizeof(uint32_t), (uint32_t)ncpu);
    result_map = map_create(BPF_MAP_TYPE_ARRAY, sizeof(uint64_t), 1);
    if (perf_map < 0 || result_map < 0) {
        printf("BPF_MAP_CREATE failed errno=%d\n", errno);
        return 1;
    }
    int prog_fd = load_program(perf_map, result_map);
    if (prog_fd < 0) {
        printf("BPF_PROG_LOAD failed errno=%d\n", errno);
        return 1;
    }
    int link_fd = attach_sys_enter(prog_fd);
    if (link_fd < 0) {
        printf("BPF_RAW_TRACEPOINT_OPEN failed errno=%d\n", errno);
        return 1;
    }

    int failures = check_targets((uint32_t)tp_id);
    failures += close_while_helpers_run((uint32_t)tp_id);

    close(link_fd);
    close(prog_fd);
    close(result_map);
    close(perf_map);
    if (failures != 0) {
        return 1;
    }
    printf("STARRY_BPF_PERF_EVENT_OUTPUT_OK\n");
    return 0;
}
