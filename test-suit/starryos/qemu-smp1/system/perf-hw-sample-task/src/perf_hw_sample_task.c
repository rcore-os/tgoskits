/*
 * perf_hw_sample_task.c -- per-task `perf record`-style SAMPLING ABI test.
 *
 * Goal (M3-pt-rec): prove per-task sampling works end to end through StarryOS,
 * exactly the way `perf record -- cmd` drives the kernel, but without depending
 * on the upstream perf binary's auxiliary requirements (cpuid sysfs, sideband
 * events). It validates: a sampling event attached to ANOTHER task (pid>0,
 * cpu=-1) with enable_on_exec starts counting at that task's execve, follows the
 * task across context switches, overflows every sample_period events, and the
 * kernel writes PERF_RECORD_SAMPLE records into the event's mmap ring.
 *
 * Sequence (mirrors perf's fork/go-pipe/exec dance):
 *   1. fork() a child; the child blocks reading a pipe, then execs itself in
 *      "--busy" mode (a long busy loop).
 *   2. the PARENT opens a SAMPLING event on the child (pid=child, cpu=-1) with
 *      PERF_TYPE_RAW / config=0x11 (ARM CPU_CYCLES, counts under QEMU TCG),
 *      sample_period set, sample_type=PERF_SAMPLE_IP, disabled=1 AND
 *      enable_on_exec=1 (so the kernel starts the counter at the child's exec).
 *   3. the parent mmaps the ring (1 header + 8 data pages), then releases the
 *      child via the pipe. The child execs -> enable_on_exec arms the per-task
 *      counter -> the busy loop overflows the period many times -> the kernel
 *      writes samples into the ring (attributed to the child while it runs).
 *   4. the parent waitpid()s the child, then walks the ring and counts
 *      PERF_RECORD_SAMPLE records.
 *
 * SUCCESS == fd>=0 AND mmap ok AND data_head!=data_tail AND >=1 sample record
 * AND a non-zero sampled ip. Prints the single sentinel STARRY_PERF_SAMPLE_TASK_OK.
 *
 * perf_event_attr / perf_event_mmap_page layouts and ioctl numbers are
 * byte-identical to the perf-hw-sample (M2) case; only the (pid, enable_on_exec)
 * attach differs. Everything is defined locally (no <linux/perf_event.h>).
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
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
#ifndef PERF_SAMPLE_IP
#define PERF_SAMPLE_IP (1ull << 0)
#endif
#define SAMPLE_PERIOD 50000ull

#ifndef PERF_EVENT_IOC_ENABLE
#define PERF_EVENT_IOC_ENABLE 0x2400u
#endif
#ifndef PERF_EVENT_IOC_DISABLE
#define PERF_EVENT_IOC_DISABLE 0x2401u
#endif
#ifndef PERF_RECORD_SAMPLE
#define PERF_RECORD_SAMPLE 9u
#endif

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
    uint64_t flags; /* bit 0 disabled; bit 12 enable_on_exec; bit 10 freq */
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

#define PERF_ATTR_FLAG_DISABLED (1ull << 0)
#define PERF_ATTR_FLAG_ENABLE_ON_EXEC (1ull << 12)

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
    uint64_t data_head;   /* @ 1024 */
    uint64_t data_tail;   /* @ 1032 */
    uint64_t data_offset; /* @ 1040 */
    uint64_t data_size;   /* @ 1048 */
    uint64_t aux_head;
    uint64_t aux_tail;
    uint64_t aux_offset;
    uint64_t aux_size;
};

_Static_assert(offsetof(struct perf_event_attr, sample_period) == 16, "sp@16");
_Static_assert(offsetof(struct perf_event_attr, sample_type) == 24, "st@24");
_Static_assert(offsetof(struct perf_event_attr, flags) == 40, "flags@40");
_Static_assert(offsetof(struct perf_event_mmap_page, data_head) == 1024, "dh@1024");
_Static_assert(offsetof(struct perf_event_mmap_page, data_tail) == 1032, "dt@1032");
_Static_assert(offsetof(struct perf_event_mmap_page, data_offset) == 1040, "do@1040");
_Static_assert(offsetof(struct perf_event_mmap_page, data_size) == 1048, "ds@1048");

struct perf_event_header {
    uint32_t type;
    uint16_t misc;
    uint16_t size;
};

#ifndef SYS_perf_event_open
#define SYS_perf_event_open 241
#endif

#define PERF_MMAP_PAGE_SIZE 4096u
#define PERF_MMAP_DATA_PAGES 8u
#define PERF_MMAP_TOTAL_BYTES                                                   \
    ((size_t)(1u + PERF_MMAP_DATA_PAGES) * PERF_MMAP_PAGE_SIZE)

static long perf_event_open(struct perf_event_attr *attr, pid_t pid, int cpu,
                            int group_fd, unsigned long flags) {
    return syscall(SYS_perf_event_open, attr, pid, cpu, group_fd, flags);
}

static int fail(const char *reason) {
    printf("perf-sample-task FAILED: %s\n", reason);
    return 1;
}

int main(int argc, char **argv) {
    /* Post-exec child: burn cycles so the attached counter overflows often. */
    if (argc > 1 && strcmp(argv[1], "--busy") == 0) {
        volatile uint64_t spin = 0;
        for (uint64_t i = 0; i < 300000000ull; i++) {
            spin += i;
        }
        return (int)(spin & 1);
    }

    /* go-pipe: child blocks until the parent has the event + mmap ready. */
    int go[2];
    if (pipe(go) != 0) {
        return fail("pipe");
    }

    pid_t child = fork();
    if (child < 0) {
        return fail("fork");
    }
    if (child == 0) {
        /* Child: wait for the go byte, then exec self in --busy mode so
         * enable_on_exec arms the parent's per-task counter at this execve. */
        close(go[1]);
        char b;
        (void)!read(go[0], &b, 1);
        close(go[0]);
        char *av[] = {argv[0], (char *)"--busy", NULL};
        execv(argv[0], av);
        _exit(127);
    }

    /* Parent: open a SAMPLING event ATTACHED TO THE CHILD with enable_on_exec. */
    close(go[0]);

    struct perf_event_attr attr;
    for (size_t i = 0; i < sizeof(attr); i++) {
        ((volatile unsigned char *)&attr)[i] = 0;
    }
    attr.type = PERF_TYPE_RAW;
    attr.config = ARM_PMU_EVT_CPU_CYCLES;
    attr.size = (uint32_t)sizeof(struct perf_event_attr);
    attr.sample_period = SAMPLE_PERIOD;
    attr.sample_type = PERF_SAMPLE_IP;
    attr.read_format = 0;
    attr.flags = PERF_ATTR_FLAG_DISABLED | PERF_ATTR_FLAG_ENABLE_ON_EXEC;

    long fd = perf_event_open(&attr, child, -1, -1, 0ul);
    if (fd < 0) {
        char msg[96];
        snprintf(msg, sizeof(msg), "perf_event_open(pid=%d) errno=%d", child, errno);
        /* release the child so it does not hang, then reap. */
        (void)!write(go[1], "g", 1);
        close(go[1]);
        waitpid(child, NULL, 0);
        return fail(msg);
    }
    int efd = (int)fd;

    void *base = mmap(NULL, PERF_MMAP_TOTAL_BYTES, PROT_READ | PROT_WRITE,
                      MAP_SHARED, efd, 0);
    if (base == MAP_FAILED) {
        int e = errno;
        char msg[96];
        snprintf(msg, sizeof(msg), "mmap ring errno=%d", e);
        (void)!write(go[1], "g", 1);
        close(go[1]);
        waitpid(child, NULL, 0);
        close(efd);
        return fail(msg);
    }
    struct perf_event_mmap_page *meta = (struct perf_event_mmap_page *)base;

    /* Release the child: it execs (enable_on_exec arms the counter) and runs. */
    (void)!write(go[1], "g", 1);
    close(go[1]);

    int status = 0;
    waitpid(child, &status, 0);
    (void)ioctl(efd, PERF_EVENT_IOC_DISABLE, 0);

    uint64_t data_head = meta->data_head;
    __sync_synchronize();
    uint64_t data_tail = meta->data_tail;
    uint64_t data_offset = meta->data_offset;
    uint64_t data_size = meta->data_size;
    const uint8_t *data_base = (const uint8_t *)base + data_offset;

    uint64_t sample_count = 0;
    uint64_t first_ip = 0;
    int saw_truncated = 0;
    uint64_t off = data_tail;
    while (off < data_head && data_size != 0) {
        uint64_t rel = off % data_size;
        struct perf_event_header hdr;
        for (size_t b = 0; b < sizeof(hdr); b++) {
            ((uint8_t *)&hdr)[b] = data_base[(rel + b) % data_size];
        }
        if (hdr.size == 0 || off + hdr.size > data_head) {
            saw_truncated = 1;
            break;
        }
        if (hdr.type == PERF_RECORD_SAMPLE) {
            uint64_t ip = 0;
            uint64_t body = rel + sizeof(hdr);
            for (size_t b = 0; b < sizeof(ip); b++) {
                ((uint8_t *)&ip)[b] = data_base[(body + b) % data_size];
            }
            if (sample_count == 0) {
                first_ip = ip;
            }
            sample_count++;
        }
        off += hdr.size;
    }

    printf("STARRY_PERF_SAMPLE_TASK count=%llu first_ip=0x%llx data_head=%llu "
           "data_tail=%llu data_size=%llu truncated=%d child_status=%d\n",
           (unsigned long long)sample_count, (unsigned long long)first_ip,
           (unsigned long long)data_head, (unsigned long long)data_tail,
           (unsigned long long)data_size, saw_truncated, status);

    int rc = 0;
    if (data_head == data_tail) {
        rc = fail("no samples captured (data_head == data_tail)");
    } else if (sample_count == 0) {
        rc = fail("no PERF_RECORD_SAMPLE records in ring");
    } else if (first_ip == 0) {
        rc = fail("sampled ip is zero");
    }

    (void)munmap(base, PERF_MMAP_TOTAL_BYTES);
    close(efd);

    if (rc == 0) {
        printf("STARRY_PERF_SAMPLE_TASK_OK\n");
        return 0;
    }
    return rc;
}
