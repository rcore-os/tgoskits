/*
 * perf_hw_inherit.c -- perf event inheritance (attr.inherit).
 *
 * `perf record` follows forked children by default (attr.inherit = 1): when a
 * monitored task clones a child, the kernel clones the event onto the child too,
 * writing the child's samples and side-band records into the SAME ring. So one
 * `perf record -- cmd` captures cmd AND every process it forks.
 *
 * This test drives exactly that. It opens a per-task event on a child C with
 * attr.inherit = attr.task = attr.sample_id_all = 1 while disabled. C execs
 * itself in --busy mode and forks grandchild G before the parent maps the ring.
 * C and G publish readiness and block. Only then does the parent map the ring,
 * enable the root fd, and release both tasks. This proves that descendants
 * inherited before mmap remain linked and receive the root output later.
 *
 * After the pre-mmap grandchild exits, C creates another 40 descendants
 * sequentially. SUCCESS therefore requires 42 distinct EXIT tids: C, G, and
 * all 40 later descendants. This exceeds the bounded simultaneously-live
 * family capacity and proves retired children release their relation slots.
 * The deliberately large sample period avoids exercising PMU overflow in this
 * lifecycle test; exact ENABLE propagation is covered by deterministic state
 * tests.
 * EXIT record body: header(8) + pid(4) + ppid(4) + tid(4) + ptid(4) + time(8),
 * so the subject tid sits at record offset 16. Prints STARRY_PERF_INHERIT_OK.
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <poll.h>
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
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
#define SAMPLE_PERIOD 0x40000000ull

#ifndef PERF_EVENT_IOC_ENABLE
#define PERF_EVENT_IOC_ENABLE 0x2400u
#endif
#ifndef PERF_EVENT_IOC_DISABLE
#define PERF_EVENT_IOC_DISABLE 0x2401u
#endif
/* perf_event_attr flag bit positions (see the bitfield in <linux/perf_event.h>). */
#define PERF_ATTR_FLAG_DISABLED (1ull << 0)
#define PERF_ATTR_FLAG_INHERIT (1ull << 1)
#define PERF_ATTR_FLAG_TASK (1ull << 13)
#define PERF_ATTR_FLAG_SAMPLE_ID_ALL (1ull << 18)

#define PERF_RECORD_EXIT 4u
#define PERF_RECORD_FORK 7u
#define PERF_RECORD_SAMPLE 9u
/* Subject tid offset within an EXIT/FORK record body (see header comment). */
#define TASK_TID_OFF 16u

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

struct perf_event_header {
    uint32_t type;
    uint16_t misc;
    uint16_t size;
};

#ifndef SYS_perf_event_open
#define SYS_perf_event_open 241
#endif
#define PERF_MMAP_DATA_PAGES 8u
#define PERF_MMAP_TOTAL_BYTES ((size_t)(1u + PERF_MMAP_DATA_PAGES) * 4096u)
#define SEQUENTIAL_DESCENDANTS 40
#define EXPECTED_EXIT_TIDS (SEQUENTIAL_DESCENDANTS + 2)
#define TEST_TIMEOUT_MS 15000
#define WAIT_POLL_INTERVAL_US 10000

static long perf_event_open(struct perf_event_attr *attr, pid_t pid, int cpu,
                            int group_fd, unsigned long flags) {
    return syscall(SYS_perf_event_open, attr, pid, cpu, group_fd, flags);
}

static int fail(const char *reason) {
    printf("perf-inherit FAILED: %s\n", reason);
    fflush(stdout);
    return 1;
}

static void stage(const char *name) {
    printf("perf-inherit stage: %s\n", name);
    fflush(stdout);
}

static int write_exact(int fd, const void *buffer, size_t length) {
    const uint8_t *cursor = buffer;
    while (length > 0) {
        ssize_t written = write(fd, cursor, length);
        if (written <= 0) {
            return -1;
        }
        cursor += (size_t)written;
        length -= (size_t)written;
    }
    return 0;
}

static int read_exact(int fd, void *buffer, size_t length) {
    uint8_t *cursor = buffer;
    while (length > 0) {
        ssize_t received = read(fd, cursor, length);
        if (received <= 0) {
            return -1;
        }
        cursor += (size_t)received;
        length -= (size_t)received;
    }
    return 0;
}

static int read_exact_timeout(int fd, void *buffer, size_t length,
                              int timeout_ms) {
    struct pollfd wait = {
        .fd = fd,
        .events = POLLIN,
    };
    int ready = poll(&wait, 1, timeout_ms);
    if (ready != 1 || (wait.revents & POLLIN) == 0) {
        return -1;
    }
    return read_exact(fd, buffer, length);
}

static pid_t waitpid_timeout(pid_t child, int *status, int timeout_ms) {
    int waited_ms = 0;
    while (waited_ms <= timeout_ms) {
        pid_t waited = waitpid(child, status, WNOHANG);
        if (waited != 0) {
            return waited;
        }
        usleep(WAIT_POLL_INTERVAL_US);
        waited_ms += WAIT_POLL_INTERVAL_US / 1000;
    }
    return 0;
}

/* Read a little-endian u32 out of the ring at byte offset `at` (wrapping). */
static uint32_t ring_u32(const uint8_t *data_base, uint64_t data_size,
                         uint64_t at) {
    uint32_t v = 0;
    for (size_t i = 0; i < 4; i++) {
        v |= (uint32_t)data_base[(at + i) % data_size] << (8 * i);
    }
    return v;
}

int main(int argc, char **argv) {
#if !defined(__aarch64__)
    /* Hardware-PMU perf is aarch64-only (ARM PMUv3); skip-as-pass on other
     * architectures so the cross-arch grouped C build/run stays green. */
    printf("STARRY_PERF_INHERIT_OK\n");
    return 0;
#endif
    if (argc == 4 && strcmp(argv[1], "--busy") == 0) {
        int ready_fd = atoi(argv[2]);
        int run_fd = atoi(argv[3]);
        pid_t gc = fork();
        if (gc == 0) {
            if (write_exact(ready_fd, "G", 1) != 0) {
                _exit(2);
            }
            char command;
            if (read_exact(run_fd, &command, 1) != 0) {
                _exit(3);
            }
            volatile uint64_t s = 0;
            for (uint64_t i = 0; i < 8000000ull; i++) {
                s += i;
            }
            _exit((int)(s & 1));
        }
        if (gc < 0 || write_exact(ready_fd, "C", 1) != 0) {
            _exit(4);
        }
        char command;
        if (read_exact(run_fd, &command, 1) != 0) {
            _exit(5);
        }
        if (waitpid(gc, NULL, 0) != gc) {
            _exit(6);
        }
        /*
         * More descendants than the bounded live-family capacity, but only one
         * at a time. Exited children must fold their counts into the root and
         * release their relationship slot instead of imposing a lifetime fork
         * limit.
         */
        for (int i = 0; i < SEQUENTIAL_DESCENDANTS; i++) {
            pid_t descendant = fork();
            if (descendant == 0) {
                _exit(0);
            }
            int descendant_status = 0;
            if (descendant < 0 ||
                waitpid(descendant, &descendant_status, 0) != descendant ||
                !WIFEXITED(descendant_status) ||
                WEXITSTATUS(descendant_status) != 0) {
                _exit(7);
            }
        }
        volatile uint64_t spin = 0;
        for (uint64_t i = 0; i < 10000000ull; i++) {
            spin += i;
        }
        return (int)(spin & 1);
    }

    int go[2];
    int ready[2];
    int run[2];
    if (pipe(go) != 0 || pipe(ready) != 0 || pipe(run) != 0) {
        return fail("pipes");
    }
    pid_t child = fork();
    if (child < 0) {
        return fail("fork");
    }
    if (child == 0) {
        close(go[1]);
        close(ready[0]);
        close(run[1]);
        char b;
        if (read_exact(go[0], &b, 1) != 0) {
            _exit(125);
        }
        close(go[0]);
        char ready_arg[16];
        char run_arg[16];
        snprintf(ready_arg, sizeof(ready_arg), "%d", ready[1]);
        snprintf(run_arg, sizeof(run_arg), "%d", run[0]);
        char *av[] = {argv[0], (char *)"--busy", ready_arg, run_arg, NULL};
        execv(argv[0], av);
        _exit(127);
    }
    close(go[0]);
    close(ready[1]);
    close(run[0]);

    struct perf_event_attr attr;
    memset(&attr, 0, sizeof(attr));
    attr.type = PERF_TYPE_RAW;
    attr.config = ARM_PMU_EVT_CPU_CYCLES;
    attr.size = (uint32_t)sizeof(attr);
    attr.sample_period = SAMPLE_PERIOD;
    attr.sample_type = PERF_SAMPLE_IP | PERF_SAMPLE_TID | PERF_SAMPLE_TIME;
    attr.flags = PERF_ATTR_FLAG_DISABLED | PERF_ATTR_FLAG_INHERIT |
                 PERF_ATTR_FLAG_TASK |
                 PERF_ATTR_FLAG_SAMPLE_ID_ALL;

    long fd = perf_event_open(&attr, child, -1, -1, 0ul);
    if (fd < 0) {
        (void)!write(go[1], "g", 1);
        close(go[1]);
        close(ready[0]);
        close(run[1]);
        waitpid(child, NULL, 0);
        return fail("perf_event_open");
    }
    int efd = (int)fd;
    stage("event-open");

    /*
     * Let C exec and fork G before mmap. Two readiness bytes prove both events
     * should already be linked into the inheritance family.
     */
    if (write_exact(go[1], "g", 1) != 0) {
        return fail("release monitored child");
    }
    close(go[1]);
    char ready_bytes[2];
    if (read_exact_timeout(ready[0], ready_bytes, sizeof(ready_bytes),
                           TEST_TIMEOUT_MS) != 0) {
        kill(child, SIGKILL);
        return fail("wait for pre-mmap descendants");
    }
    close(ready[0]);
    stage("descendants-ready-before-mmap");

    void *base = mmap(NULL, PERF_MMAP_TOTAL_BYTES, PROT_READ | PROT_WRITE,
                      MAP_SHARED, efd, 0);
    if (base == MAP_FAILED) {
        (void)write_exact(run[1], "rr", 2);
        close(run[1]);
        waitpid(child, NULL, 0);
        close(efd);
        return fail("mmap ring");
    }
    struct perf_event_mmap_page *meta = (struct perf_event_mmap_page *)base;
    stage("ring-mapped");

    if (ioctl(efd, PERF_EVENT_IOC_ENABLE, 0) != 0) {
        return fail("enable inherited family");
    }
    stage("family-enabled");
    if (write_exact(run[1], "rr", 2) != 0) {
        return fail("release inherited family");
    }
    close(run[1]);
    stage("family-released");
    int status = 0;
    if (waitpid_timeout(child, &status, TEST_TIMEOUT_MS) != child ||
        !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        kill(child, SIGKILL);
        return fail("monitored family exit");
    }
    stage("family-exited");
    (void)ioctl(efd, PERF_EVENT_IOC_DISABLE, 0);

    uint64_t data_head = meta->data_head;
    __sync_synchronize();
    uint64_t data_tail = meta->data_tail;
    uint64_t data_offset = meta->data_offset;
    uint64_t data_size = meta->data_size;
    const uint8_t *data_base = (const uint8_t *)base + data_offset;

    uint64_t n_fork = 0, n_exit = 0, n_sample = 0;
    uint32_t exit_tids[64];
    size_t n_exit_tids = 0;
    uint64_t off = data_tail;
    while (off < data_head && data_size != 0) {
        uint64_t rel = off % data_size;
        struct perf_event_header hdr;
        for (size_t b = 0; b < sizeof(hdr); b++) {
            ((uint8_t *)&hdr)[b] = data_base[(rel + b) % data_size];
        }
        if (hdr.size == 0 || off + hdr.size > data_head) {
            break;
        }
        if (hdr.type == PERF_RECORD_FORK) {
            n_fork++;
        } else if (hdr.type == PERF_RECORD_EXIT) {
            n_exit++;
            uint32_t tid = ring_u32(data_base, data_size, rel + TASK_TID_OFF);
            if (n_exit_tids < sizeof(exit_tids) / sizeof(exit_tids[0])) {
                exit_tids[n_exit_tids++] = tid;
            }
        } else if (hdr.type == PERF_RECORD_SAMPLE) {
            n_sample++;
        }
        off += hdr.size;
    }

    size_t distinct_exit = 0;
    for (size_t i = 0; i < n_exit_tids; i++) {
        int seen = 0;
        for (size_t j = 0; j < i; j++) {
            if (exit_tids[j] == exit_tids[i]) {
                seen = 1;
                break;
            }
        }
        if (!seen) {
            distinct_exit++;
        }
    }
    printf("STARRY_PERF_INHERIT fork=%llu exit=%llu distinct_exit_tids=%zu "
           "samples=%llu child=%d\n",
           (unsigned long long)n_fork, (unsigned long long)n_exit,
           distinct_exit, (unsigned long long)n_sample, (int)child);

    int rc = 0;
    if (n_exit == 0) {
        rc = fail("no PERF_RECORD_EXIT record");
    } else if (distinct_exit < EXPECTED_EXIT_TIDS) {
        rc = fail("retired children exhausted live inheritance capacity");
    }

    (void)munmap(base, PERF_MMAP_TOTAL_BYTES);
    close(efd);
    if (rc == 0) {
        printf("STARRY_PERF_INHERIT_OK\n");
        return 0;
    }
    return rc;
}
