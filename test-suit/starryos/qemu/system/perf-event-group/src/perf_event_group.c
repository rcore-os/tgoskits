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

static int open_sw(uint64_t config, uint64_t read_format, int group_fd) {
    struct perf_event_attr_v0 attr = {
        .type = PERF_TYPE_SOFTWARE,
        .size = sizeof(attr),
        .config = config,
        .read_format = read_format,
        .flags = PERF_ATTR_DISABLED,
    };
    return (int)syscall(SYS_PERF_EVENT_OPEN, &attr, 0, -1, group_fd, 0ul);
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

    /* A fixed-CPU hardware group is scheduled and read as one unit. Closing
     * its leader must leave a live member usable as a standalone event. */
    leader = open_system_raw(format, PERF_ATTR_DISABLED, -1);
    member = open_system_raw(0, PERF_ATTR_DISABLED, leader);
    if (leader < 0 || member < 0 || ioctl(leader, PERF_IOC_ID, &leader_id) != 0 ||
        ioctl(member, PERF_IOC_ID, &member_id) != 0 ||
        ioctl(leader, PERF_IOC_ENABLE, PERF_IOC_FLAG_GROUP) != 0) {
        printf("perf-event-group FAILED: system group setup errno=%d\n", errno);
        return 1;
    }
    work();
    if (ioctl(leader, PERF_IOC_DISABLE, PERF_IOC_FLAG_GROUP) != 0 ||
        read(leader, values, sizeof(values)) != (ssize_t)sizeof(values) ||
        values[0] != 2 || values[3] == 0 || values[4] != leader_id ||
        values[5] == 0 || values[6] != member_id) {
        puts("perf-event-group FAILED: system group snapshot");
        return 1;
    }
    close(leader);
    if (ioctl(member, PERF_IOC_ENABLE, 0) != 0) {
        puts("perf-event-group FAILED: system member after leader close");
        return 1;
    }
    work();
    if (read(member, &member_value, sizeof(member_value)) !=
            (ssize_t)sizeof(member_value) ||
        member_value == 0) {
        puts("perf-event-group FAILED: system member read after leader close");
        return 1;
    }
    close(member);

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
     * Establish the disabled task event in the current CPU context, then
     * enable it while all programmable slots are occupied by CPU-flexible
     * events. Linux reschedules immediately and gives pinned the slot. */
    int task_pinned = open_task_raw(PERF_ATTR_DISABLED | PERF_ATTR_PINNED);
    if (task_pinned < 0 || sched_yield() != 0 ||
        ioctl(task_pinned, PERF_IOC_ENABLE, 0) != 0) {
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

    /* A pinned group larger than the six A53 programmable counters must fail
     * as a complete transaction and expose Linux's pinned ERROR/EOF state. */
    int pinned[7];
    pinned[0] = open_system_raw(0, PERF_ATTR_DISABLED | PERF_ATTR_PINNED, -1);
    for (int i = 1; i < 7; ++i) {
        pinned[i] = open_system_raw(0, PERF_ATTR_DISABLED, pinned[0]);
    }
    errno = 0;
    if (pinned[0] < 0 ||
        ioctl(pinned[0], PERF_IOC_ENABLE, PERF_IOC_FLAG_GROUP) == 0 ||
        errno != EBUSY || read(pinned[0], &member_value, sizeof(member_value)) != 0) {
        printf("perf-event-group FAILED: pinned overcommit errno=%d\n", errno);
        return 1;
    }
    for (int i = 6; i >= 0; --i) {
        close(pinned[i]);
    }
    printf("STARRY_PERF_EVENT_GROUP nr=%llu leader=%llu member=%llu\n",
           (unsigned long long)values[0], (unsigned long long)values[3],
           (unsigned long long)member_value);
    puts("STARRY_PERF_EVENT_GROUP_OK");
    return 0;
#endif
}
