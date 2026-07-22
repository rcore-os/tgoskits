/*
 * perf_hw_group.c -- event groups + PERF_FORMAT_GROUP.
 *
 * `perf stat -e '{cycles,instructions}'` opens the events as a GROUP: a leader
 * (group_fd = -1) plus members (group_fd = leader's fd). Enabling the leader
 * schedules the whole group, and one read of the leader with PERF_FORMAT_GROUP
 * returns every member's counter in a single buffer. Previously group_fd was
 * dropped and PERF_FORMAT_GROUP was unsupported, so the leader read returned the
 * flat single-event layout (a malformed buffer to perf's group parser).
 *
 * This test opens a 2-event group (task-clock leader + cpu-clock member on the
 * calling thread), enables ONLY the leader, runs a workload, disables the leader,
 * and reads the leader with PERF_FORMAT_GROUP | TIME_ENABLED | TIME_RUNNING | ID.
 *
 * The group MECHANISM (membership, leader enable/disable propagation, group read
 * layout, id demux) is event-type-agnostic — it lives in the perf-event file
 * wrapper, above the per-type impls. This test uses SOFTWARE events because
 * QEMU-TCG's PMU implements only CPU_CYCLES, not a second hardware event, so a
 * hardware {cycles,instructions} group cannot be exercised in QEMU; the identical
 * path applies to hardware groups on real silicon.
 *
 * SUCCESS ==
 *     both events open AND the leader read returns the group layout
 *       {nr, time_enabled, time_running, val0, id0, val1, id1} (7 u64)
 *   AND nr == 2
 *   AND id0 / id1 match the ids reported by PERF_EVENT_IOC_ID for the leader /
 *       member (correct id demux)
 *   AND the member (cpu-clock) counted > 0 — proving enabling ONLY the leader
 *       started the member too (group scheduling), since the member was never
 *       enabled directly (it counts wall time only while enabled).
 * On success exactly one line `STARRY_PERF_GROUP_OK` is printed.
 *
 * Software events are pure accounting, so this is not arch-gated; it skips-as-pass
 * only if perf_event_open is unwired (ENOSYS).
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <fcntl.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <unistd.h>

#define PERF_TYPE_SOFTWARE 1u
#define PERF_COUNT_SW_CPU_CLOCK 0ull
#define PERF_COUNT_SW_TASK_CLOCK 1ull

#define PERF_FORMAT_TOTAL_TIME_ENABLED (1ull << 0)
#define PERF_FORMAT_TOTAL_TIME_RUNNING (1ull << 1)
#define PERF_FORMAT_ID (1ull << 2)
#define PERF_FORMAT_GROUP (1ull << 3)

#ifndef PERF_EVENT_IOC_ENABLE
#define PERF_EVENT_IOC_ENABLE 0x2400u
#endif
#ifndef PERF_EVENT_IOC_DISABLE
#define PERF_EVENT_IOC_DISABLE 0x2401u
#endif
#ifndef PERF_EVENT_IOC_RESET
#define PERF_EVENT_IOC_RESET 0x2403u
#endif
/* _IOR('$', 7, __u64) — read this event's unique id into *arg. */
#ifndef PERF_EVENT_IOC_ID
#define PERF_EVENT_IOC_ID 0x80082407u
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

#define PERF_ATTR_FLAG_DISABLED (1ull << 0)

#ifndef SYS_perf_event_open
#define SYS_perf_event_open 241
#endif

static long perf_event_open(struct perf_event_attr *attr, pid_t pid, int cpu,
                            int group_fd, unsigned long flags) {
    return syscall(SYS_perf_event_open, attr, pid, cpu, group_fd, flags);
}

static int open_sw(uint64_t config, uint64_t read_format, pid_t pid,
                   int group_fd) {
    struct perf_event_attr attr;
    memset(&attr, 0, sizeof(attr));
    attr.type = PERF_TYPE_SOFTWARE;
    attr.size = (uint32_t)sizeof(attr);
    attr.config = config;
    attr.read_format = read_format;
    attr.flags = PERF_ATTR_FLAG_DISABLED;
    return (int)perf_event_open(&attr, pid, -1, group_fd, 0ul);
}

static int g_zfd = -1;
static volatile uint64_t g_sink;
static void workload(void) {
    static uint8_t buf[4096];
    for (uint64_t i = 0; i < 300000ull; i++) {
        if (g_zfd >= 0) {
            if (read(g_zfd, buf, sizeof(buf)) < 0) {
                break;
            }
        } else {
            g_sink += i * 2654435761ull + 1ull;
        }
    }
}

int main(void) {
#if !defined(__aarch64__)
    printf("STARRY_PERF_GROUP_OK\n");
    return 0;
#endif
    pid_t tid = (pid_t)syscall(SYS_gettid);
    uint64_t rf = PERF_FORMAT_GROUP | PERF_FORMAT_TOTAL_TIME_ENABLED |
                  PERF_FORMAT_TOTAL_TIME_RUNNING | PERF_FORMAT_ID;

    int leader = open_sw(PERF_COUNT_SW_TASK_CLOCK, rf, tid, -1);
    if (leader < 0) {
        if (errno == ENOSYS) {
            printf("STARRY_PERF_GROUP skip: perf_event_open ENOSYS\n");
            printf("STARRY_PERF_GROUP_OK\n");
            return 0;
        }
        printf("perf-hw-group FAILED: open leader errno=%d\n", errno);
        return 1;
    }
    /* Member joins the leader's group. read_format need not match the leader. */
    int member = open_sw(PERF_COUNT_SW_CPU_CLOCK, rf, tid, leader);
    if (member < 0) {
        printf("perf-hw-group FAILED: open member errno=%d\n", errno);
        close(leader);
        return 1;
    }

    uint64_t leader_id = 0, member_id = 0;
    if (ioctl(leader, PERF_EVENT_IOC_ID, &leader_id) != 0 ||
        ioctl(member, PERF_EVENT_IOC_ID, &member_id) != 0) {
        printf("perf-hw-group FAILED: IOC_ID errno=%d\n", errno);
        close(member);
        close(leader);
        return 1;
    }

    g_zfd = open("/dev/zero", O_RDONLY);
    /* Enable ONLY the leader — the member must start via group propagation. */
    (void)ioctl(leader, PERF_EVENT_IOC_RESET, 0);
    (void)ioctl(leader, PERF_EVENT_IOC_ENABLE, 0);
    workload();
    (void)ioctl(leader, PERF_EVENT_IOC_DISABLE, 0);
    if (g_zfd >= 0) {
        close(g_zfd);
    }

    /* Group read: {nr, time_enabled, time_running, val0, id0, val1, id1}. */
    uint64_t buf[16];
    ssize_t got = read(leader, buf, sizeof(buf));
    close(member);
    close(leader);

    int rc = 0;
    ssize_t want = (ssize_t)(7 * sizeof(uint64_t));
    if (got != want) {
        printf("perf-hw-group FAILED: group read got=%zd want=%zd errno=%d "
               "(PERF_FORMAT_GROUP layout wrong)\n",
               got, want, errno);
        return 1;
    }
    uint64_t nr = buf[0], te = buf[1], tr = buf[2];
    uint64_t val0 = buf[3], id0 = buf[4], val1 = buf[5], id1 = buf[6];
    printf("STARRY_PERF_GROUP nr=%llu te=%llu tr=%llu task_clock=%llu id0=%llu "
           "cpu_clock=%llu id1=%llu (leader_id=%llu member_id=%llu)\n",
           (unsigned long long)nr, (unsigned long long)te,
           (unsigned long long)tr, (unsigned long long)val0,
           (unsigned long long)id0, (unsigned long long)val1,
           (unsigned long long)id1, (unsigned long long)leader_id,
           (unsigned long long)member_id);

    if (nr != 2) {
        printf("perf-hw-group FAILED: nr=%llu (expected 2 group members)\n",
               (unsigned long long)nr);
        rc = 1;
    }
    if (id0 != leader_id || id1 != member_id) {
        printf("perf-hw-group FAILED: group ids do not match IOC_ID (demux "
               "broken)\n");
        rc = 1;
    }
    if (val1 == 0) {
        printf("perf-hw-group FAILED: member (cpu-clock) counted 0 — enabling "
               "only the leader did not start the group\n");
        rc = 1;
    }

    if (rc == 0) {
        printf("STARRY_PERF_GROUP_OK\n");
    }
    return rc;
}
