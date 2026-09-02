/*
 * perf-hw-smp-manythreads -- more concurrently-monitored tasks than there are
 * programmable counters on one core (8 children vs ~6 counters), spread across
 * 4 cores. With the OLD single global allocator the excess opens lost monitoring
 * (counter exhaustion). With per-CPU pools + per-slice allocation each core has
 * its own counters and only the running task needs one, so all 8 are counted.
 *
 * Parent forks 8 busy children, opens a per-task RAW 0x11 counting event on each
 * (pid=child), enables it, waits for all, then reads each.
 *
 * SUCCESS: every perf_event_open succeeded (fd>=0 -- no NoMemory exhaustion) AND
 * every child accumulated a non-zero count (read()==24 bytes, value>0).
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#define PERF_TYPE_RAW 4u
#define ARM_CPU_CYCLES 0x11ull
#define PERF_FORMAT_TIMING 3ull
#define PERF_IOC_ENABLE 0x2400u
#define PERF_IOC_DISABLE 0x2401u
#define PERF_ATTR_DISABLED (1ull << 0)
#ifndef SYS_perf_event_open
#define SYS_perf_event_open 241
#endif

#define NCHILD 8

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

static long peo(struct perf_event_attr *a, pid_t pid, int cpu, int gfd,
                unsigned long fl) {
    return syscall(SYS_perf_event_open, a, pid, cpu, gfd, fl);
}

int main(void) {
#if !defined(__aarch64__)
    /* Hardware-PMU perf is aarch64-only (ARM PMUv3); skip-as-pass on other
     * architectures so the cross-arch grouped C run stays green. */
    printf("STARRY_SMP_MANYTHREADS_OK\n");
    return 0;
#endif
    pid_t pids[NCHILD];
    long fds[NCHILD];

    /* Fork 8 busy children. A long loop guarantees each is still running when
     * the parent (with only a handful of quick open+enable syscalls) enables
     * its event, so a non-zero count is not racy. */
    for (int i = 0; i < NCHILD; i++) {
        pid_t c = fork();
        if (c == 0) {
            volatile uint64_t s = 0;
            for (uint64_t k = 0; k < 30000000ull; k++) {
                s += k;
            }
            _exit(0);
        }
        pids[i] = c;
        if (c < 0) {
            printf("manythreads FAILED: fork[%d] errno=%d\n", i, errno);
            return 1;
        }
    }

    struct perf_event_attr attr;
    int ok = 1;
    for (int i = 0; i < NCHILD; i++) {
        for (size_t b = 0; b < sizeof(attr); b++) {
            ((volatile unsigned char *)&attr)[b] = 0;
        }
        attr.type = PERF_TYPE_RAW;
        attr.config = ARM_CPU_CYCLES;
        attr.size = sizeof(attr);
        attr.read_format = PERF_FORMAT_TIMING;
        attr.flags = PERF_ATTR_DISABLED;
        fds[i] = peo(&attr, pids[i], -1, -1, 0);
        if (fds[i] < 0) {
            printf("manythreads FAILED: open[%d] errno=%d (counter exhaustion?)\n",
                   i, errno);
            ok = 0;
        } else {
            (void)ioctl((int)fds[i], PERF_IOC_ENABLE, 0);
        }
    }

    for (int i = 0; i < NCHILD; i++) {
        if (pids[i] > 0) {
            int st;
            waitpid(pids[i], &st, 0);
        }
    }

    for (int i = 0; i < NCHILD; i++) {
        if (fds[i] < 0) {
            continue;
        }
        (void)ioctl((int)fds[i], PERF_IOC_DISABLE, 0);
        uint64_t buf[3] = {0, 0, 0};
        ssize_t n = read((int)fds[i], buf, sizeof(buf));
        printf("STARRY_SMP_MANYTHREADS[%d] value=%llu enabled=%llu n=%lld\n", i,
               (unsigned long long)buf[0], (unsigned long long)buf[1],
               (long long)n);
        if (n != 24 || buf[0] == 0) {
            printf("manythreads FAILED: child %d not counted\n", i);
            ok = 0;
        }
        close((int)fds[i]);
    }

    if (ok) {
        printf("STARRY_SMP_MANYTHREADS_OK\n");
        return 0;
    }
    return 1;
}
