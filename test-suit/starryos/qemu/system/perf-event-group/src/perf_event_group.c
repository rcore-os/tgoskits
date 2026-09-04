/* Linux v7.1 event-group control, read order, context, and lifetime test. */
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

#define PERF_TYPE_SOFTWARE 1u
#define PERF_COUNT_SW_CPU_CLOCK 0u
#define PERF_COUNT_SW_TASK_CLOCK 1u
#define PERF_FORMAT_TOTAL_TIME_ENABLED (1ull << 0)
#define PERF_FORMAT_TOTAL_TIME_RUNNING (1ull << 1)
#define PERF_FORMAT_ID (1ull << 2)
#define PERF_FORMAT_GROUP (1ull << 3)
#define PERF_ATTR_DISABLED (1ull << 0)
#define PERF_IOC_ENABLE 0x2400u
#define PERF_IOC_DISABLE 0x2401u
#define PERF_IOC_ID 0x80082407u
#define SYS_PERF_EVENT_OPEN 241

struct perf_event_attr_v0 {
    uint32_t type, size;
    uint64_t config, sample_period, sample_type, read_format, flags;
    uint32_t wakeup_events, bp_type;
    uint64_t bp_addr;
};

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

static void work(void) {
    for (uint64_t i = 0; i < 8000000; i++) {
        sink += (i * 5u) ^ sink;
    }
}

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

    if (ioctl(leader, PERF_IOC_ENABLE, 0) != 0) {
        puts("perf-event-group FAILED: enable");
        return 1;
    }
    work();
    if (ioctl(leader, PERF_IOC_DISABLE, 0) != 0) {
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
    printf("STARRY_PERF_EVENT_GROUP nr=%llu leader=%llu member=%llu\n",
           (unsigned long long)values[0], (unsigned long long)values[3],
           (unsigned long long)member_value);
    puts("STARRY_PERF_EVENT_GROUP_OK");
    return 0;
#endif
}
