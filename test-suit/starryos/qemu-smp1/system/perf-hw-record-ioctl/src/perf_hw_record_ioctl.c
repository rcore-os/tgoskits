/*
 * perf_hw_record_ioctl.c -- `perf record` ring-setup ioctl ABI test.
 *
 * Regression guard for the perf-record mmap path. After mmap(perf_fd), the real
 * `perf record` issues two ioctls the kernel previously rejected with EINVAL,
 * which perf reports as the misleading "failed to mmap with 22":
 *
 *   - PERF_EVENT_IOC_ID  (_IOR('$', 7, __u64 *)): perf reads back the event's
 *     unique id (to build its id->event map) right after mmap. Rejecting it
 *     aborts `perf record` even though the mmap itself succeeded.
 *   - PERF_EVENT_IOC_SET_OUTPUT (_IO('$', 5)): perf points a second event (its
 *     PERF_COUNT_SW_DUMMY tracking event, opened for `perf record -a`) at the
 *     leader's mmap ring so they share one buffer.
 *
 * This test drives exactly that sequence without depending on the upstream perf
 * binary: open a per-task SAMPLING event, mmap the ring, IOC_ID it (expect a
 * unique non-zero id), open a second (dummy) event, SET_OUTPUT it onto the
 * leader, IOC_ID the second (expect a *different* non-zero id), then run the
 * child and confirm the ring still captured samples.
 *
 * SUCCESS == both IOC_IDs return distinct non-zero ids AND SET_OUTPUT returns 0
 * AND the ring captured >=1 PERF_RECORD_SAMPLE. Prints the single sentinel
 * STARRY_PERF_RECORD_IOCTL_OK.
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

#ifndef PERF_TYPE_HARDWARE
#define PERF_TYPE_HARDWARE 0u
#endif
#ifndef PERF_TYPE_SOFTWARE
#define PERF_TYPE_SOFTWARE 1u
#endif
#ifndef PERF_TYPE_RAW
#define PERF_TYPE_RAW 4u
#endif
#ifndef PERF_COUNT_HW_CPU_CYCLES
#define PERF_COUNT_HW_CPU_CYCLES 0u
#endif
#ifndef PERF_COUNT_SW_DUMMY
#define PERF_COUNT_SW_DUMMY 9u
#endif
#ifndef ARM_PMU_EVT_CPU_CYCLES
#define ARM_PMU_EVT_CPU_CYCLES 0x11ull
#endif
#ifndef PERF_SAMPLE_IP
#define PERF_SAMPLE_IP (1ull << 0)
#endif
#define SAMPLE_PERIOD 50000ull

#ifndef PERF_EVENT_IOC_DISABLE
#define PERF_EVENT_IOC_DISABLE 0x2401u
#endif
/* _IOR('$', 7, __u64 *): kernel writes the event's unique id to *arg. */
#ifndef PERF_EVENT_IOC_ID
#define PERF_EVENT_IOC_ID _IOR('$', 7, uint64_t *)
#endif
/* _IO('$', 5): redirect this event's records into the fd-named event's ring. */
#ifndef PERF_EVENT_IOC_SET_OUTPUT
#define PERF_EVENT_IOC_SET_OUTPUT _IO('$', 5)
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
    uint64_t flags; /* bit 0 disabled; bit 12 enable_on_exec */
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
    uint64_t data_head;
    uint64_t data_tail;
    uint64_t data_offset;
    uint64_t data_size;
    uint64_t aux_head;
    uint64_t aux_tail;
    uint64_t aux_offset;
    uint64_t aux_size;
};

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

static void zero_attr(struct perf_event_attr *attr) {
    for (size_t i = 0; i < sizeof(*attr); i++) {
        ((volatile unsigned char *)attr)[i] = 0;
    }
    attr->size = (uint32_t)sizeof(struct perf_event_attr);
}

static int fail(const char *reason) {
    printf("perf-record-ioctl FAILED: %s\n", reason);
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

    int go[2];
    if (pipe(go) != 0) {
        return fail("pipe");
    }
    pid_t child = fork();
    if (child < 0) {
        return fail("fork");
    }
    if (child == 0) {
        close(go[1]);
        char b;
        (void)!read(go[0], &b, 1);
        close(go[0]);
        char *av[] = {argv[0], (char *)"--busy", NULL};
        execv(argv[0], av);
        _exit(127);
    }
    close(go[0]);

    /* Leader: a per-task SAMPLING cycles event with enable_on_exec (perf-style). */
    struct perf_event_attr attr;
    zero_attr(&attr);
    attr.type = PERF_TYPE_RAW;
    attr.config = ARM_PMU_EVT_CPU_CYCLES;
    attr.sample_period = SAMPLE_PERIOD;
    attr.sample_type = PERF_SAMPLE_IP;
    attr.flags = PERF_ATTR_FLAG_DISABLED | PERF_ATTR_FLAG_ENABLE_ON_EXEC;

    long lead = perf_event_open(&attr, child, -1, -1, 0ul);
    if (lead < 0) {
        (void)!write(go[1], "g", 1);
        close(go[1]);
        waitpid(child, NULL, 0);
        return fail("perf_event_open(leader)");
    }
    int lfd = (int)lead;

    void *base = mmap(NULL, PERF_MMAP_TOTAL_BYTES, PROT_READ | PROT_WRITE,
                      MAP_SHARED, lfd, 0);
    if (base == MAP_FAILED) {
        (void)!write(go[1], "g", 1);
        close(go[1]);
        waitpid(child, NULL, 0);
        close(lfd);
        return fail("mmap ring");
    }
    struct perf_event_mmap_page *meta = (struct perf_event_mmap_page *)base;

    /* (1) PERF_EVENT_IOC_ID on the leader -- the call that perf issues right
     * after mmap and that previously failed with EINVAL. */
    uint64_t lead_id = 0;
    if (ioctl(lfd, PERF_EVENT_IOC_ID, &lead_id) != 0) {
        int e = errno;
        char msg[96];
        snprintf(msg, sizeof(msg), "PERF_EVENT_IOC_ID(leader) errno=%d", e);
        (void)!write(go[1], "g", 1);
        close(go[1]);
        waitpid(child, NULL, 0);
        return fail(msg);
    }
    if (lead_id == 0) {
        (void)!write(go[1], "g", 1);
        close(go[1]);
        waitpid(child, NULL, 0);
        return fail("leader id is zero");
    }

    /* (2) A second (dummy) event + PERF_EVENT_IOC_SET_OUTPUT onto the leader --
     * the multi-event ring-sharing call perf uses for `perf record -a`. */
    struct perf_event_attr dattr;
    zero_attr(&dattr);
    dattr.type = PERF_TYPE_SOFTWARE;
    dattr.config = PERF_COUNT_SW_DUMMY;
    dattr.sample_period = SAMPLE_PERIOD;
    dattr.sample_type = PERF_SAMPLE_IP;
    dattr.flags = PERF_ATTR_FLAG_DISABLED;

    long dummy = perf_event_open(&dattr, child, -1, -1, 0ul);
    if (dummy < 0) {
        (void)!write(go[1], "g", 1);
        close(go[1]);
        waitpid(child, NULL, 0);
        return fail("perf_event_open(dummy)");
    }
    int dfd = (int)dummy;

    if (ioctl(dfd, PERF_EVENT_IOC_SET_OUTPUT, lfd) != 0) {
        int e = errno;
        char msg[96];
        snprintf(msg, sizeof(msg), "PERF_EVENT_IOC_SET_OUTPUT errno=%d", e);
        (void)!write(go[1], "g", 1);
        close(go[1]);
        waitpid(child, NULL, 0);
        return fail(msg);
    }

    uint64_t dummy_id = 0;
    if (ioctl(dfd, PERF_EVENT_IOC_ID, &dummy_id) != 0 || dummy_id == 0) {
        (void)!write(go[1], "g", 1);
        close(go[1]);
        waitpid(child, NULL, 0);
        return fail("PERF_EVENT_IOC_ID(dummy)");
    }
    if (dummy_id == lead_id) {
        (void)!write(go[1], "g", 1);
        close(go[1]);
        waitpid(child, NULL, 0);
        return fail("event ids are not unique");
    }

    /* Release the child: it execs (enable_on_exec arms the counter) and runs. */
    (void)!write(go[1], "g", 1);
    close(go[1]);

    int status = 0;
    waitpid(child, &status, 0);
    (void)ioctl(lfd, PERF_EVENT_IOC_DISABLE, 0);

    /* Confirm the ring still captured samples after the ioctl dance. */
    uint64_t data_head = meta->data_head;
    __sync_synchronize();
    uint64_t data_tail = meta->data_tail;
    uint64_t data_offset = meta->data_offset;
    uint64_t data_size = meta->data_size;
    const uint8_t *data_base = (const uint8_t *)base + data_offset;

    uint64_t sample_count = 0;
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
        if (hdr.type == PERF_RECORD_SAMPLE) {
            sample_count++;
        }
        off += hdr.size;
    }

    printf("STARRY_PERF_RECORD_IOCTL lead_id=%llu dummy_id=%llu samples=%llu "
           "child_status=%d\n",
           (unsigned long long)lead_id, (unsigned long long)dummy_id,
           (unsigned long long)sample_count, status);

    int rc = 0;
    if (sample_count == 0) {
        rc = fail("no PERF_RECORD_SAMPLE records after ioctl setup");
    }

    (void)munmap(base, PERF_MMAP_TOTAL_BYTES);
    close(dfd);
    close(lfd);

    if (rc == 0) {
        printf("STARRY_PERF_RECORD_IOCTL_OK\n");
        return 0;
    }
    return rc;
}
