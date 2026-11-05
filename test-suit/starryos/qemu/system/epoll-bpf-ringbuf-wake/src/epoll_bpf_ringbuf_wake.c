/*
 * epoll interests woken by bpf_ringbuf_output while the epoll is closed.
 *
 * A raw sys_enter program commits a ringbuf record for one chosen thread, so
 * that thread's next syscall wakes the epoll interests on the ringbuf fd from
 * inside the helper, with local IRQs disabled. The wake must reach the interest
 * it was registered for, also after EPOLL_CTL_MOD replaces it. The callback
 * must never release the last reference to an epoll: the main thread closes
 * the epoll fd while the wake runs, and the epoll also watches pipe read ends
 * whose wait lock other threads keep busy, so destroying the epoll in the
 * callback would sleep with IRQs disabled.
 *
 * The decisive steps of that race are atomic operations inside the kernel
 * (see JITTER_MAX_NS), so the close/wake overlap stays a per-round race that
 * userspace cannot turn into an ordered interleave; the loop instead fails a
 * run that stops before CLOSE_MIN_ROUNDS rounds.
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#ifndef SYS_bpf
#error "SYS_bpf is required"
#endif

#define BPF_MAP_CREATE 0
#define BPF_MAP_LOOKUP_ELEM 1
#define BPF_MAP_UPDATE_ELEM 2
#define BPF_PROG_LOAD 5
#define BPF_RAW_TRACEPOINT_OPEN 17
#define BPF_MAP_TYPE_ARRAY 2
#define BPF_MAP_TYPE_RINGBUF 27
#define BPF_PROG_TYPE_TRACEPOINT 5
#define BPF_PSEUDO_MAP_FD 1

#define BPF_LD 0x00
#define BPF_LDX 0x01
#define BPF_ST 0x02
#define BPF_STX 0x03
#define BPF_JMP 0x05
#define BPF_ALU64 0x07
#define BPF_W 0x00
#define BPF_DW 0x18
#define BPF_IMM 0x00
#define BPF_MEM 0x60
#define BPF_K 0x00
#define BPF_X 0x08
#define BPF_ADD 0x00
#define BPF_LSH 0x60
#define BPF_RSH 0x70
#define BPF_MOV 0xb0
#define BPF_JEQ 0x10
#define BPF_JNE 0x50
#define BPF_CALL 0x80
#define BPF_EXIT 0x90

#define HELPER_MAP_LOOKUP_ELEM 1
#define HELPER_GET_CURRENT_PID_TGID 14
#define HELPER_RINGBUF_OUTPUT 130
#define BPF_RB_FORCE_WAKEUP 2

/* Pipe read ends watched by each epoll, and threads polling that pipe. */
#define PIPE_INTERESTS 16
#define PIPE_POLLERS 2
/* Rounds of wake-and-close; the time limit bounds slow emulation. */
#define CLOSE_ROUNDS 2048
/*
 * Rounds that a run stopped by the time limit must still have completed.
 * The unfixed kernel panicked between rounds 512 and 576 at the latest
 * across ten local runs (QEMU TCG, -smp 4), so 1024 keeps about 1.8x
 * margin over the latest observed hit. A run below this count has not
 * covered the known trigger range and fails instead of reporting success.
 */
#define CLOSE_MIN_ROUNDS 1024
/*
 * Full runs need about 15 s for 2048 rounds on the reference QEMU runner,
 * so 30 s tolerates a slower one while staying far below the 120 s the
 * system test runner allows per program; no per-case timeout entry needed.
 */
#define CLOSE_SECONDS 30
/* Lets the notification worker drop the reference EPOLL_CTL_ADD handed it. */
#define SETTLE_US 200
/*
 * The close is delayed by up to this long after the wake is released, so it
 * runs while the waker thread is inside its syscall. The handoff of the last
 * reference cannot be ordered from userspace: it happens between two atomic
 * steps inside the kernel, the callback dropping its temporary reference in
 * the IRQ-disabled ringbuf wake and the close path dropping the file's
 * reference. Userspace can only order the two syscall entries, so every
 * round retries this bounded race and the round minimum above is what turns
 * a too-slow run into a failure.
 */
#define JITTER_MAX_NS 8000

struct bpf_insn {
    uint8_t code;
    uint8_t regs;
    int16_t off;
    int32_t imm;
};

static long sys_bpf(int cmd, void *attr, unsigned int size) {
    return syscall(SYS_bpf, cmd, attr, size);
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

static int map_create(uint32_t type, uint32_t key_size, uint32_t value_size,
                      uint32_t entries) {
    struct {
        uint32_t map_type;
        uint32_t key_size;
        uint32_t value_size;
        uint32_t max_entries;
        uint32_t map_flags;
    } attr = {
        .map_type = type,
        .key_size = key_size,
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

/*
 * if (bpf_get_current_pid_tgid() == *tid_map[0]) {
 *     *result_map[0] = bpf_ringbuf_output(ringbuf, &zero, 8, FORCE_WAKEUP);
 * }
 */
static int load_program(int tid_map, int ringbuf, int result_map) {
    struct bpf_insn prog[] = {
        insn(BPF_JMP | BPF_CALL, 0, 0, 0, HELPER_GET_CURRENT_PID_TGID),
        insn(BPF_ALU64 | BPF_MOV | BPF_X, 7, 0, 0, 0),
        insn(BPF_LD | BPF_DW | BPF_IMM, 1, BPF_PSEUDO_MAP_FD, 0, tid_map),
        insn(0, 0, 0, 0, 0),
        insn(BPF_ALU64 | BPF_MOV | BPF_X, 2, 10, 0, 0),
        insn(BPF_ALU64 | BPF_ADD | BPF_K, 2, 0, 0, -4),
        insn(BPF_ST | BPF_MEM | BPF_W, 10, 0, -4, 0),
        insn(BPF_JMP | BPF_CALL, 0, 0, 0, HELPER_MAP_LOOKUP_ELEM),
        insn(BPF_JMP | BPF_JEQ | BPF_K, 0, 0, 19, 0),
        insn(BPF_LDX | BPF_MEM | BPF_DW, 8, 0, 0, 0),
        insn(BPF_JMP | BPF_JNE | BPF_X, 7, 8, 17, 0),
        insn(BPF_LD | BPF_DW | BPF_IMM, 1, BPF_PSEUDO_MAP_FD, 0, ringbuf),
        insn(0, 0, 0, 0, 0),
        insn(BPF_ALU64 | BPF_MOV | BPF_X, 2, 10, 0, 0),
        insn(BPF_ALU64 | BPF_ADD | BPF_K, 2, 0, 0, -16),
        insn(BPF_ST | BPF_MEM | BPF_DW, 10, 0, -16, 0),
        insn(BPF_ALU64 | BPF_MOV | BPF_K, 3, 0, 0, 8),
        insn(BPF_ALU64 | BPF_MOV | BPF_K, 4, 0, 0, BPF_RB_FORCE_WAKEUP),
        insn(BPF_JMP | BPF_CALL, 0, 0, 0, HELPER_RINGBUF_OUTPUT),
        insn(BPF_ALU64 | BPF_MOV | BPF_X, 6, 0, 0, 0),
        insn(BPF_LD | BPF_DW | BPF_IMM, 1, BPF_PSEUDO_MAP_FD, 0, result_map),
        insn(0, 0, 0, 0, 0),
        insn(BPF_ALU64 | BPF_MOV | BPF_X, 2, 10, 0, 0),
        insn(BPF_ALU64 | BPF_ADD | BPF_K, 2, 0, 0, -8),
        insn(BPF_ST | BPF_MEM | BPF_W, 10, 0, -8, 0),
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
        .prog_type = BPF_PROG_TYPE_TRACEPOINT,
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

static int ringbuf;
static int result_map;
static int pipe_rd[PIPE_INTERESTS];

#define GO_STOP UINT64_MAX

static uint64_t go;
static uint64_t fired;
static int stop_pollers;
static double spins_per_ns;

/* Commits one ringbuf record from its next syscall each time `go` advances. */
static void *wake_thread(void *arg) {
    int tid_map = *(int *)arg;
    uint64_t pid_tgid = ((uint64_t)getpid() << 32) | (uint64_t)gettid();
    if (map_elem(BPF_MAP_UPDATE_ELEM, tid_map, 0, &pid_tgid) != 0) {
        printf("tid map update failed errno=%d\n", errno);
        __atomic_store_n(&fired, GO_STOP, __ATOMIC_RELEASE);
        return NULL;
    }
    __atomic_store_n(&fired, 0, __ATOMIC_RELEASE);
    uint64_t seen = 0;
    for (;;) {
        uint64_t next;
        while ((next = __atomic_load_n(&go, __ATOMIC_ACQUIRE)) == seen) {
        }
        if (next == GO_STOP) {
            return NULL;
        }
        seen = next;
        syscall(SYS_getpid);
        __atomic_store_n(&fired, seen, __ATOMIC_RELEASE);
    }
}

static void *pipe_poller(void *arg) {
    struct pollfd pfd = {.fd = *(int *)arg, .events = POLLIN};
    while (!__atomic_load_n(&stop_pollers, __ATOMIC_RELAXED)) {
        poll(&pfd, 1, 0);
    }
    return NULL;
}

static void fire_wake(void) {
    uint64_t round = __atomic_add_fetch(&go, 1, __ATOMIC_ACQ_REL);
    while (__atomic_load_n(&fired, __ATOMIC_ACQUIRE) != round) {
    }
}

static void spin_ns(unsigned long ns) {
    unsigned long spins = (unsigned long)((double)ns * spins_per_ns);
    for (volatile unsigned long i = 0; i < spins; i++) {
    }
}

static long elapsed_ns(const struct timespec *start) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (now.tv_sec - start->tv_sec) * 1000000000L + (now.tv_nsec - start->tv_nsec);
}

static void calibrate_spin(void) {
    const unsigned long spins = 1000000;
    struct timespec start;
    clock_gettime(CLOCK_MONOTONIC, &start);
    for (volatile unsigned long i = 0; i < spins; i++) {
    }
    long ns = elapsed_ns(&start);
    spins_per_ns = ns > 0 ? (double)spins / (double)ns : 1.0;
}

static int add_interest(int epfd, int fd, uint32_t events, uint64_t data) {
    struct epoll_event ev = {.events = events, .data.u64 = data};
    if (epoll_ctl(epfd, EPOLL_CTL_ADD, fd, &ev) != 0) {
        printf("EPOLL_CTL_ADD fd=%d failed errno=%d\n", fd, errno);
        return -1;
    }
    return 0;
}

/* Expect exactly `want` events within `timeout_ms`, all carrying `data`. */
static int expect_events(int epfd, int want, uint64_t data, int timeout_ms, const char *what) {
    struct epoll_event out[4];
    int n = epoll_wait(epfd, out, 4, timeout_ms);
    if (n != want || (n == 1 && out[0].data.u64 != data)) {
        printf("%s: epoll_wait=%d data=0x%llx, expected %d event(s) with 0x%llx\n", what,
               n, n > 0 ? (unsigned long long)out[0].data.u64 : 0ull, want,
               (unsigned long long)data);
        return 1;
    }
    return 0;
}

static int check_wake_delivery(void) {
    int failures = 0;
    int epfd = epoll_create1(0);
    if (epfd < 0 || add_interest(epfd, ringbuf, EPOLLIN | EPOLLET, 0xa1) != 0) {
        return 1;
    }
    failures += expect_events(epfd, 0, 0, 0, "empty ringbuf");
    fire_wake();
    failures += expect_events(epfd, 1, 0xa1, 1000, "first ringbuf record");
    failures += expect_events(epfd, 0, 0, 0, "edge already reported");

    struct epoll_event ev = {.events = EPOLLIN | EPOLLET, .data.u64 = 0xb2};
    if (epoll_ctl(epfd, EPOLL_CTL_MOD, ringbuf, &ev) != 0) {
        printf("EPOLL_CTL_MOD failed errno=%d\n", errno);
        return failures + 1;
    }
    failures += expect_events(epfd, 1, 0xb2, 1000, "readable ringbuf after MOD");
    fire_wake();
    failures += expect_events(epfd, 1, 0xb2, 1000, "record after MOD");

    if (epoll_ctl(epfd, EPOLL_CTL_DEL, ringbuf, NULL) != 0) {
        printf("EPOLL_CTL_DEL failed errno=%d\n", errno);
        return failures + 1;
    }
    fire_wake();
    failures += expect_events(epfd, 0, 0, 0, "record after DEL");
    close(epfd);
    return failures;
}

static uint64_t next_random(void) {
    static uint64_t state = 88172645463325252ull;
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    return state;
}

static int close_while_waking(void) {
    pthread_t pollers[PIPE_POLLERS];
    for (int i = 0; i < PIPE_POLLERS; i++) {
        if (pthread_create(&pollers[i], NULL, pipe_poller, &pipe_rd[i]) != 0) {
            printf("pthread_create failed\n");
            return 1;
        }
    }
    int failures = 0;
    int rounds = 0;
    struct timespec start;
    clock_gettime(CLOCK_MONOTONIC, &start);
    for (; rounds < CLOSE_ROUNDS && elapsed_ns(&start) < CLOSE_SECONDS * 1000000000L; rounds++) {
        int epfd = epoll_create1(0);
        if (epfd < 0) {
            printf("round %d: epoll_create1 failed errno=%d\n", rounds, errno);
            failures++;
            break;
        }
        int added = add_interest(epfd, ringbuf, EPOLLIN, 0) == 0;
        for (int i = 0; added && i < PIPE_INTERESTS; i++) {
            added = add_interest(epfd, pipe_rd[i], EPOLLIN, (uint64_t)i + 1) == 0;
        }
        if (!added) {
            failures++;
            break;
        }
        usleep(SETTLE_US);
        unsigned long jitter = (unsigned long)(next_random() % (JITTER_MAX_NS + 1));
        uint64_t round = __atomic_add_fetch(&go, 1, __ATOMIC_ACQ_REL);
        spin_ns(jitter);
        if (close(epfd) != 0) {
            printf("round %d: close failed errno=%d\n", rounds, errno);
            failures++;
            break;
        }
        while (__atomic_load_n(&fired, __ATOMIC_ACQUIRE) != round) {
        }
    }
    long spent_ns = elapsed_ns(&start);
    __atomic_store_n(&stop_pollers, 1, __ATOMIC_RELAXED);
    for (int i = 0; i < PIPE_POLLERS; i++) {
        pthread_join(pollers[i], NULL);
    }
    printf("closed %d epolls while their ringbuf interest was woken\n", rounds);
    if (spent_ns >= CLOSE_SECONDS * 1000000000L && rounds < CLOSE_MIN_ROUNDS) {
        printf("time limit reached after only %d rounds, below the minimum of %d\n", rounds,
               CLOSE_MIN_ROUNDS);
        failures++;
    }
    return failures;
}

int main(void) {
    ringbuf = map_create(BPF_MAP_TYPE_RINGBUF, 0, 0, 1u << 20);
    int tid_map = map_create(BPF_MAP_TYPE_ARRAY, sizeof(uint32_t), sizeof(uint64_t), 1);
    result_map = map_create(BPF_MAP_TYPE_ARRAY, sizeof(uint32_t), sizeof(uint64_t), 1);
    if (ringbuf < 0 || tid_map < 0 || result_map < 0) {
        printf("BPF_MAP_CREATE failed errno=%d\n", errno);
        return 1;
    }
    uint64_t sentinel = 0x5a5a5a5a5a5a5a5aull;
    if (map_elem(BPF_MAP_UPDATE_ELEM, result_map, 0, &sentinel) != 0) {
        printf("result map update failed errno=%d\n", errno);
        return 1;
    }
    int prog_fd = load_program(tid_map, ringbuf, result_map);
    if (prog_fd < 0) {
        printf("BPF_PROG_LOAD failed errno=%d\n", errno);
        return 1;
    }
    int link_fd = attach_sys_enter(prog_fd);
    if (link_fd < 0) {
        printf("BPF_RAW_TRACEPOINT_OPEN failed errno=%d\n", errno);
        return 1;
    }

    int pipefd[2];
    if (pipe(pipefd) != 0) {
        printf("pipe failed errno=%d\n", errno);
        return 1;
    }
    pipe_rd[0] = pipefd[0];
    for (int i = 1; i < PIPE_INTERESTS; i++) {
        pipe_rd[i] = fcntl(pipefd[0], F_DUPFD, 0);
        if (pipe_rd[i] < 0) {
            printf("F_DUPFD failed errno=%d\n", errno);
            return 1;
        }
    }

    calibrate_spin();
    __atomic_store_n(&fired, GO_STOP - 1, __ATOMIC_RELEASE);
    pthread_t waker;
    if (pthread_create(&waker, NULL, wake_thread, &tid_map) != 0) {
        printf("pthread_create failed\n");
        return 1;
    }
    uint64_t ready;
    while ((ready = __atomic_load_n(&fired, __ATOMIC_ACQUIRE)) == GO_STOP - 1) {
    }
    if (ready == GO_STOP) {
        return 1;
    }

    int failures = check_wake_delivery();
    failures += close_while_waking();

    __atomic_store_n(&go, GO_STOP, __ATOMIC_RELEASE);
    pthread_join(waker, NULL);
    uint64_t helper_ret = 0;
    if (map_elem(BPF_MAP_LOOKUP_ELEM, result_map, 0, &helper_ret) != 0 || helper_ret != 0) {
        printf("bpf_ringbuf_output returned %lld, expected 0\n", (long long)helper_ret);
        failures++;
    }

    close(link_fd);
    close(prog_fd);
    close(result_map);
    close(tid_map);
    close(ringbuf);
    if (failures != 0) {
        return 1;
    }
    printf("STARRY_EPOLL_BPF_RINGBUF_WAKE_OK\n");
    return 0;
}
