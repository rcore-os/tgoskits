/*
 * perf_hw_group_crosstask.c -- group-leader sampling same-thread safety gate.
 *
 * A per-task sampling leader opened with PERF_FORMAT_GROUP bakes raw pointers to
 * each counting member's PerTaskCounter atomics into its per-CPU SampleSlot at
 * slice-arm time; that registration's lifetime is bounded to the LEADER's slice.
 * The invariant that makes those raw pointers sound is that every member lives in
 * the SAME thread's counter list as the leader (so it outlives the slot). A
 * member on a DIFFERENT thread could be freed (its thread exits, fd closed) while
 * the leader's slot still references it -> use-after-free in the hard-IRQ overflow
 * handler. The kernel therefore rejects a cross-thread group link with EINVAL,
 * mirroring Linux's group_leader->ctx == event->ctx check.
 *
 * Flow: a helper thread B publishes its tid and waits. The main thread opens a
 * sampling leader on ITSELF (PERF_FORMAT_GROUP), then attempts two member opens
 * with group_fd = leader:
 *   - member on B's tid   -> MUST fail with EINVAL (cross-thread rejected)
 *   - member on own tid   -> MUST succeed (same-thread accepted, control)
 *
 * SUCCESS ==
 *     leader open succeeds
 *   AND the cross-thread member open fails with errno == EINVAL
 *   AND the same-thread member open succeeds.
 * On success exactly one line `STARRY_PERF_GROUP_CROSSTASK_OK` is printed.
 *
 * aarch64-only (ARM PMUv3); skips-as-pass elsewhere.
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <pthread.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef PERF_TYPE_RAW
#define PERF_TYPE_RAW 4u
#endif
#ifndef ARM_PMU_EVT_CPU_CYCLES
#define ARM_PMU_EVT_CPU_CYCLES 0x11ull
#endif

#define PERF_SAMPLE_IP (1ull << 0)
#define PERF_SAMPLE_TID (1ull << 1)
#define PERF_SAMPLE_READ (1ull << 10)

#define PERF_FORMAT_ID (1ull << 2)
#define PERF_FORMAT_GROUP (1ull << 3)

#define SAMPLE_PERIOD 100000ull
#define PERF_ATTR_FLAG_DISABLED (1ull << 0)

#ifndef SYS_perf_event_open
#define SYS_perf_event_open 241
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

static long perf_event_open(struct perf_event_attr *attr, pid_t pid, int cpu,
                            int group_fd, unsigned long flags) {
    return syscall(SYS_perf_event_open, attr, pid, cpu, group_fd, flags);
}

static void attr_zero(struct perf_event_attr *a) {
    for (size_t i = 0; i < sizeof(*a); i++) {
        ((volatile unsigned char *)a)[i] = 0;
    }
    a->type = PERF_TYPE_RAW;
    a->config = ARM_PMU_EVT_CPU_CYCLES;
    a->size = (uint32_t)sizeof(*a);
    a->flags = PERF_ATTR_FLAG_DISABLED;
}

struct shared {
    volatile pid_t b_tid;
    volatile int stop;
};

static void *worker_b(void *arg) {
    struct shared *s = (struct shared *)arg;
    s->b_tid = (pid_t)syscall(SYS_gettid);
    __sync_synchronize();
    /* Stay alive (distinct live tid) while main performs the group opens. */
    while (!s->stop) {
        for (volatile int d = 0; d < 100000; d++) {
        }
    }
    return NULL;
}

int main(void) {
#if !defined(__aarch64__)
    printf("STARRY_PERF_GROUP_CROSSTASK_OK\n");
    return 0;
#endif
    struct shared s = {0, 0};
    pthread_t b;
    if (pthread_create(&b, NULL, worker_b, &s) != 0) {
        printf("perf-group-crosstask FAILED: pthread_create\n");
        return 1;
    }
    /* Wait for B to publish its tid. */
    while (s.b_tid == 0) {
        for (volatile int d = 0; d < 1000; d++) {
        }
    }
    __sync_synchronize();

    pid_t self_tid = (pid_t)syscall(SYS_gettid);
    int rc = 0;

    /* Sampling leader on ourselves, group read layout. */
    struct perf_event_attr lattr;
    attr_zero(&lattr);
    lattr.sample_period = SAMPLE_PERIOD;
    lattr.sample_type = PERF_SAMPLE_IP | PERF_SAMPLE_TID | PERF_SAMPLE_READ;
    lattr.read_format = PERF_FORMAT_GROUP | PERF_FORMAT_ID;
    long lfd = perf_event_open(&lattr, self_tid, -1, -1, 0ul);
    if (lfd < 0) {
        if (errno == ENOSYS) {
            printf("STARRY_PERF_GROUP_CROSSTASK skip: ENOSYS\n");
            printf("STARRY_PERF_GROUP_CROSSTASK_OK\n");
            s.stop = 1;
            pthread_join(b, NULL);
            return 0;
        }
        printf("perf-group-crosstask FAILED: leader open errno=%d\n", errno);
        s.stop = 1;
        pthread_join(b, NULL);
        return 1;
    }
    int leader = (int)lfd;

    /* Cross-thread member (pid = B's tid): MUST be rejected with EINVAL. */
    struct perf_event_attr mattr;
    attr_zero(&mattr);
    errno = 0;
    long xfd = perf_event_open(&mattr, s.b_tid, -1, leader, 0ul);
    int xerr = errno;
    if (xfd >= 0) {
        printf("perf-group-crosstask FAILED: cross-thread member linked (fd=%ld) "
               "-- UAF gate missing\n",
               xfd);
        close((int)xfd);
        rc = 1;
    } else if (xerr != EINVAL) {
        printf("perf-group-crosstask FAILED: cross-thread member rejected with "
               "errno=%d, expected EINVAL(%d)\n",
               xerr, EINVAL);
        rc = 1;
    }

    /* Same-thread member (pid = own tid): MUST succeed (control). */
    struct perf_event_attr sattr;
    attr_zero(&sattr);
    long sfd = perf_event_open(&sattr, self_tid, -1, leader, 0ul);
    if (sfd < 0) {
        printf("perf-group-crosstask FAILED: same-thread member rejected "
               "errno=%d\n",
               errno);
        rc = 1;
    } else {
        close((int)sfd);
    }

    printf("STARRY_PERF_GROUP_CROSSTASK cross_fd=%ld cross_errno=%d\n", xfd, xerr);

    close(leader);
    s.stop = 1;
    pthread_join(b, NULL);

    if (rc == 0) {
        printf("STARRY_PERF_GROUP_CROSSTASK_OK\n");
    }
    return rc;
}
