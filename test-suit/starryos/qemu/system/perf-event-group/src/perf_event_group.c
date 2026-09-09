/* Linux v7.1 event-group control, read order, context, and lifetime test. */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#define PERF_TYPE_SOFTWARE 1u
#define PERF_TYPE_RAW 4u
#define PERF_COUNT_SW_CPU_CLOCK 0u
#define PERF_COUNT_SW_TASK_CLOCK 1u
#define PERF_SAMPLE_IP (1ull << 0)
#define PERF_FORMAT_TOTAL_TIME_ENABLED (1ull << 0)
#define PERF_FORMAT_TOTAL_TIME_RUNNING (1ull << 1)
#define PERF_FORMAT_ID (1ull << 2)
#define PERF_FORMAT_GROUP (1ull << 3)
#define PERF_ATTR_DISABLED (1ull << 0)
#define PERF_ATTR_PINNED (1ull << 2)
#define PERF_IOC_ENABLE 0x2400u
#define PERF_IOC_DISABLE 0x2401u
#define PERF_IOC_RESET 0x2403u
#define PERF_IOC_ID 0x80082407u
#define PERF_IOC_FLAG_GROUP (1ul << 0)
#define SYS_PERF_EVENT_OPEN 241

struct perf_event_attr_v0 {
    uint32_t type, size;
    uint64_t config, sample_period, sample_type, read_format, flags;
    uint32_t wakeup_events, bp_type;
    uint64_t bp_addr;
};

#if defined(__aarch64__)
static volatile uint64_t sink;

static int open_sw_flags(uint64_t config, uint64_t read_format, int group_fd,
                         uint64_t flags) {
    struct perf_event_attr_v0 attr = {
        .type = PERF_TYPE_SOFTWARE,
        .size = sizeof(attr),
        .config = config,
        .read_format = read_format,
        .flags = flags,
    };
    return (int)syscall(SYS_PERF_EVENT_OPEN, &attr, 0, -1, group_fd, 0ul);
}

static int open_sw(uint64_t config, uint64_t read_format, int group_fd) {
    return open_sw_flags(config, read_format, group_fd, PERF_ATTR_DISABLED);
}

static int open_system_sw_flags(uint64_t config, uint64_t read_format,
                                int group_fd, uint64_t flags) {
    struct perf_event_attr_v0 attr = {
        .type = PERF_TYPE_SOFTWARE,
        .size = sizeof(attr),
        .config = config,
        .read_format = read_format,
        .flags = flags,
    };
    return (int)syscall(SYS_PERF_EVENT_OPEN, &attr, -1, 0, group_fd, 0ul);
}

static int open_system_raw(uint64_t read_format, uint64_t flags, int group_fd) {
    struct perf_event_attr_v0 attr = {
        .type = PERF_TYPE_RAW,
        .size = sizeof(attr),
        .config = 0x11,
        .read_format = read_format,
        .flags = flags,
    };
    return (int)syscall(SYS_PERF_EVENT_OPEN, &attr, -1, 0, group_fd, 0ul);
}

static int open_system_raw_sampling(int group_fd) {
    struct perf_event_attr_v0 attr = {
        .type = PERF_TYPE_RAW,
        .size = sizeof(attr),
        .config = 0x11,
        .sample_period = 100000,
        .sample_type = PERF_SAMPLE_IP,
        .flags = PERF_ATTR_DISABLED,
    };
    return (int)syscall(SYS_PERF_EVENT_OPEN, &attr, -1, 0, group_fd, 0ul);
}

static int open_task_raw(uint64_t flags) {
    struct perf_event_attr_v0 attr = {
        .type = PERF_TYPE_RAW,
        .size = sizeof(attr),
        .config = 0x11,
        .flags = flags,
    };
    return (int)syscall(SYS_PERF_EVENT_OPEN, &attr, 0, -1, -1, 0ul);
}

static void work(void) {
    for (uint64_t i = 0; i < 8000000; i++) {
        sink += (i * 5u) ^ sink;
    }
}
#endif

int main(void) {
#if !defined(__aarch64__)
    puts("STARRY_PERF_EVENT_GROUP_OK");
    return 0;
#else
    /* An enabled sibling inherits the disabled leader's effective OFF state.
     * Enabling only the sibling cannot bypass that gate; enabling the leader
     * alone then schedules every sibling whose own state is enabled. */
    int gated_leader = open_sw(PERF_COUNT_SW_TASK_CLOCK, 0, -1);
    int eager_member = open_sw_flags(
        PERF_COUNT_SW_CPU_CLOCK,
        PERF_FORMAT_TOTAL_TIME_ENABLED | PERF_FORMAT_TOTAL_TIME_RUNNING,
        gated_leader, 0);
    if (gated_leader < 0 || eager_member < 0) {
        printf("perf-event-group FAILED: disabled leader setup errno=%d\n",
               errno);
        return 1;
    }
    uint64_t gated_values[3] = {0};
    work();
    if (read(eager_member, gated_values, sizeof(gated_values)) !=
            (ssize_t)sizeof(gated_values) ||
        gated_values[0] != 0 || gated_values[1] != 0 || gated_values[2] != 0 ||
        ioctl(eager_member, PERF_IOC_ENABLE, 0) != 0) {
        printf("perf-event-group FAILED: member bypassed disabled leader "
               "value=%llu enabled=%llu running=%llu errno=%d\n",
               (unsigned long long)gated_values[0],
               (unsigned long long)gated_values[1],
               (unsigned long long)gated_values[2], errno);
        return 1;
    }
    work();
    if (read(eager_member, gated_values, sizeof(gated_values)) !=
            (ssize_t)sizeof(gated_values) ||
        gated_values[0] != 0 || gated_values[1] != 0 || gated_values[2] != 0 ||
        ioctl(gated_leader, PERF_IOC_ENABLE, 0) != 0) {
        puts("perf-event-group FAILED: member-only enable bypassed leader");
        return 1;
    }
    work();
    if (ioctl(gated_leader, PERF_IOC_DISABLE, 0) != 0 ||
        read(eager_member, gated_values, sizeof(gated_values)) !=
            (ssize_t)sizeof(gated_values) ||
        gated_values[0] == 0 || gated_values[1] == 0 ||
        gated_values[2] == 0) {
        puts("perf-event-group FAILED: leader-only enable did not run member");
        return 1;
    }
    close(eager_member);
    close(gated_leader);

    /* The same effective-state rule applies to a fixed-CPU software context. */
    gated_leader = open_system_sw_flags(PERF_COUNT_SW_TASK_CLOCK, 0, -1,
                                        PERF_ATTR_DISABLED);
    eager_member = open_system_sw_flags(
        PERF_COUNT_SW_CPU_CLOCK,
        PERF_FORMAT_TOTAL_TIME_ENABLED | PERF_FORMAT_TOTAL_TIME_RUNNING,
        gated_leader, 0);
    if (gated_leader < 0 || eager_member < 0) {
        printf("perf-event-group FAILED: system disabled leader errno=%d\n",
               errno);
        return 1;
    }
    work();
    if (read(eager_member, gated_values, sizeof(gated_values)) !=
            (ssize_t)sizeof(gated_values) ||
        gated_values[0] != 0 || gated_values[1] != 0 || gated_values[2] != 0 ||
        ioctl(gated_leader, PERF_IOC_ENABLE, 0) != 0) {
        puts("perf-event-group FAILED: system member bypassed leader");
        return 1;
    }
    work();
    if (ioctl(gated_leader, PERF_IOC_DISABLE, 0) != 0 ||
        read(eager_member, gated_values, sizeof(gated_values)) !=
            (ssize_t)sizeof(gated_values) ||
        gated_values[0] == 0 || gated_values[1] == 0 ||
        gated_values[2] == 0) {
        puts("perf-event-group FAILED: system leader did not run member");
        return 1;
    }
    close(eager_member);
    close(gated_leader);

    const uint64_t format = PERF_FORMAT_GROUP | PERF_FORMAT_ID |
                            PERF_FORMAT_TOTAL_TIME_ENABLED |
                            PERF_FORMAT_TOTAL_TIME_RUNNING;
    int leader = open_sw(PERF_COUNT_SW_TASK_CLOCK, format, -1);
    int member = open_sw(PERF_COUNT_SW_CPU_CLOCK, 0, leader);
    if (leader < 0 || member < 0) {
        printf("perf-event-group FAILED: open errno=%d\n", errno);
        return 1;
    }
    uint64_t leader_id = 0, member_id = 0;
    if (ioctl(leader, PERF_IOC_ID, &leader_id) != 0 ||
        ioctl(member, PERF_IOC_ID, &member_id) != 0) {
        puts("perf-event-group FAILED: ids");
        return 1;
    }

    pid_t child = fork();
    if (child == 0) {
        errno = 0;
        int fd = open_sw(PERF_COUNT_SW_CPU_CLOCK, 0, leader);
        if (fd >= 0) {
            close(fd);
            _exit(2);
        }
        _exit(errno == EINVAL ? 0 : 3);
    }
    int status = 0;
    if (child < 0 || waitpid(child, &status, 0) != child ||
        !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        puts("perf-event-group FAILED: cross-task member was not EINVAL");
        return 1;
    }

    if (ioctl(leader, PERF_IOC_ENABLE, PERF_IOC_FLAG_GROUP) != 0) {
        puts("perf-event-group FAILED: enable");
        return 1;
    }
    work();
    if (ioctl(leader, PERF_IOC_DISABLE, PERF_IOC_FLAG_GROUP) != 0) {
        puts("perf-event-group FAILED: disable");
        return 1;
    }

    uint64_t values[7] = {0};
    if (read(leader, values, sizeof(values)) != (ssize_t)sizeof(values) ||
        values[0] != 2 || values[2] > values[1] || values[3] == 0 ||
        values[4] != leader_id || values[5] == 0 || values[6] != member_id) {
        printf("perf-event-group FAILED: nr=%llu enabled=%llu running=%llu "
               "leader=%llu/%llu member=%llu/%llu\n",
               (unsigned long long)values[0],
               (unsigned long long)values[1],
               (unsigned long long)values[2],
               (unsigned long long)values[3],
               (unsigned long long)values[4],
               (unsigned long long)values[5],
               (unsigned long long)values[6]);
        return 1;
    }

    /* Linux applies RESET to only the addressed event unless GROUP is set. */
    const uint64_t member_before_reset = values[5];
    if (ioctl(leader, PERF_IOC_RESET, 0) != 0 ||
        read(leader, values, sizeof(values)) != (ssize_t)sizeof(values) ||
        values[3] != 0 || values[5] != member_before_reset) {
        printf("perf-event-group FAILED: reset without GROUP leader=%llu "
               "member=%llu/%llu\n",
               (unsigned long long)values[3],
               (unsigned long long)values[5],
               (unsigned long long)member_before_reset);
        return 1;
    }
    if (ioctl(leader, PERF_IOC_RESET, PERF_IOC_FLAG_GROUP) != 0 ||
        read(leader, values, sizeof(values)) != (ssize_t)sizeof(values) ||
        values[3] != 0 || values[5] != 0) {
        puts("perf-event-group FAILED: reset with GROUP");
        return 1;
    }

    /* Closing the leader must not leave the member with a dangling owner. */
    close(leader);
    if (ioctl(member, PERF_IOC_ENABLE, 0) != 0) {
        puts("perf-event-group FAILED: member enable after leader close");
        return 1;
    }
    work();
    uint64_t member_value = 0;
    if (read(member, &member_value, sizeof(member_value)) !=
            (ssize_t)sizeof(member_value) ||
        member_value == 0) {
        puts("perf-event-group FAILED: member read after leader close");
        return 1;
    }
    close(member);

    /* Fixed-CPU flexible hardware events currently have independent workers.
     * Reject the member until one transactional group coordinator owns their
     * slots and snapshots; a file-level group alone would be misleading. */
    leader = open_system_raw(format, PERF_ATTR_DISABLED, -1);
    errno = 0;
    member = open_system_raw(0, PERF_ATTR_DISABLED, leader);
    if (leader < 0 || member >= 0 || errno != EOPNOTSUPP) {
        printf("perf-event-group FAILED: uncoordinated system group errno=%d\n",
               errno);
        if (member >= 0) {
            close(member);
        }
        if (leader >= 0) {
            close(leader);
        }
        return 1;
    }
    close(leader);

    /* Starry also lacks Linux's context migration for mixed software/hardware
     * groups. Both opening orders must fail identically instead of one order
     * publishing a no-op hardware link. */
    leader = open_system_sw_flags(PERF_COUNT_SW_CPU_CLOCK, 0, -1,
                                  PERF_ATTR_DISABLED);
    errno = 0;
    member = open_system_raw(0, PERF_ATTR_DISABLED, leader);
    if (leader < 0 || member >= 0 || errno != EOPNOTSUPP) {
        printf("perf-event-group FAILED: software/hardware group errno=%d\n",
               errno);
        return 1;
    }
    close(leader);
    leader = open_system_raw(0, PERF_ATTR_DISABLED, -1);
    errno = 0;
    member = open_system_sw_flags(PERF_COUNT_SW_CPU_CLOCK, 0, leader,
                                  PERF_ATTR_DISABLED);
    if (leader < 0 || member >= 0 || errno != EOPNOTSUPP) {
        printf("perf-event-group FAILED: hardware/software group errno=%d\n",
               errno);
        return 1;
    }
    close(leader);

    /* Direct system-wide sampling currently has no group-aware backend. It
     * must reject every mixed or sampling-only group instead of publishing a
     * file-level group whose PMU events still run independently. */
    leader = open_system_raw_sampling(-1);
    errno = 0;
    member = open_system_raw_sampling(leader);
    if (leader < 0 || member >= 0 || errno != EOPNOTSUPP) {
        printf("perf-event-group FAILED: system sampling group errno=%d\n",
               errno);
        if (member >= 0) {
            close(member);
        }
        if (leader >= 0) {
            close(leader);
        }
        return 1;
    }
    errno = 0;
    member = open_system_raw(0, PERF_ATTR_DISABLED, leader);
    if (member >= 0 || errno != EOPNOTSUPP) {
        printf("perf-event-group FAILED: sampling leader mixed group errno=%d\n",
               errno);
        if (member >= 0) {
            close(member);
        }
        close(leader);
        return 1;
    }
    close(leader);

    leader = open_system_raw(0, PERF_ATTR_DISABLED, -1);
    errno = 0;
    member = open_system_raw_sampling(leader);
    if (leader < 0 || member >= 0 || errno != EOPNOTSUPP) {
        printf("perf-event-group FAILED: sampling member mixed group errno=%d\n",
               errno);
        if (member >= 0) {
            close(member);
        }
        if (leader >= 0) {
            close(leader);
        }
        return 1;
    }
    close(leader);

    /* CPU-context pinned events outrank flexible CPU/task contexts. Filling all
     * six A53 programmable slots with flexible events must not make a later
     * pinned event fail: the scheduler first evicts flexible work, then places
     * the pinned event and refills whatever capacity remains. */
    int flexible[6];
    cpu_set_t affinity;
    CPU_ZERO(&affinity);
    CPU_SET(0, &affinity);
    if (sched_setaffinity(0, sizeof(affinity), &affinity) != 0) {
        puts("perf-event-group FAILED: pin test task to CPU0");
        return 1;
    }
    for (int i = 0; i < 6; ++i) {
        flexible[i] = open_system_raw(0, 0, -1);
        if (flexible[i] < 0) {
            printf("perf-event-group FAILED: flexible fill %d errno=%d\n", i,
                   errno);
            return 1;
        }
    }
    int pinned_one =
        open_system_raw(0, PERF_ATTR_DISABLED | PERF_ATTR_PINNED, -1);
    if (pinned_one < 0 || ioctl(pinned_one, PERF_IOC_ENABLE, 0) != 0) {
        printf("perf-event-group FAILED: pinned did not evict flexible errno=%d\n",
               errno);
        return 1;
    }
    work();
    if (ioctl(pinned_one, PERF_IOC_DISABLE, 0) != 0 ||
        read(pinned_one, &member_value, sizeof(member_value)) !=
            (ssize_t)sizeof(member_value) ||
        member_value == 0) {
        puts("perf-event-group FAILED: pinned after flexible snapshot");
        return 1;
    }
    close(pinned_one);

    /* Task-pinned has second priority, ahead of both CPU/task flexible work.
     * The target is the calling task and is already running: Linux installs
     * that task context at open, so enable must immediately reschedule the PMU
     * without requiring an unrelated context switch. */
    int task_pinned = open_task_raw(PERF_ATTR_DISABLED | PERF_ATTR_PINNED);
    if (task_pinned < 0 || ioctl(task_pinned, PERF_IOC_ENABLE, 0) != 0) {
        printf("perf-event-group FAILED: task pinned setup errno=%d\n", errno);
        return 1;
    }
    work();
    if (ioctl(task_pinned, PERF_IOC_DISABLE, 0) != 0 ||
        read(task_pinned, &member_value, sizeof(member_value)) !=
            (ssize_t)sizeof(member_value) ||
        member_value == 0) {
        puts("perf-event-group FAILED: task pinned did not evict flexible");
        return 1;
    }
    close(task_pinned);
    for (int i = 5; i >= 0; --i) {
        close(flexible[i]);
    }

    printf("STARRY_PERF_EVENT_GROUP nr=%llu leader=%llu member=%llu\n",
           (unsigned long long)values[0], (unsigned long long)values[3],
           (unsigned long long)member_value);
    puts("STARRY_PERF_EVENT_GROUP_OK");
    return 0;
#endif
}
