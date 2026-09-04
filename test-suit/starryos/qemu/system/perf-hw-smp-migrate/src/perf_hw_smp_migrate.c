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

#define PERF_TYPE_RAW 4u
#define PERF_FORMAT_TIMING 3ull
#define PERF_IOC_ENABLE 0x2400u
#define PERF_IOC_DISABLE 0x2401u
#define PERF_ATTR_DISABLED (1ull << 0)
#ifndef SYS_perf_event_open
#define SYS_perf_event_open 241
#endif

struct perf_event_attr {
    uint32_t type;
    uint32_t size;
    uint64_t config;
    uint64_t sample_period;
    uint64_t sample_type;
    uint64_t read_format;
    uint64_t flags;
    uint8_t tail[80];
};

static long perf_open(struct perf_event_attr *attr, pid_t pid, int cpu) {
    return syscall(SYS_perf_event_open, attr, pid, cpu, -1, 0);
}

static void run_child(int gate) {
    char byte;
    if (read(gate, &byte, 1) != 1) {
        _exit(2);
    }
    volatile uint64_t sum = 0;
    for (int cpu = 0; cpu < 4; ++cpu) {
        cpu_set_t set;
        CPU_ZERO(&set);
        CPU_SET(cpu, &set);
        if (sched_setaffinity(0, sizeof(set), &set) != 0) {
            _exit(3);
        }
        for (uint64_t i = 0; i < 12000000ull; ++i) {
            sum += i;
        }
    }
    _exit(sum == UINT64_MAX);
}

int main(void) {
#if !defined(__aarch64__)
    puts("STARRY_SMP_MIGRATE_OK");
    return 0;
#endif
    int gate[2];
    if (pipe(gate) != 0) {
        return 1;
    }
    pid_t child = fork();
    if (child == 0) {
        close(gate[1]);
        run_child(gate[0]);
    }
    if (child < 0) {
        return 1;
    }
    close(gate[0]);

    struct perf_event_attr attr = {
        .type = PERF_TYPE_RAW,
        .size = sizeof(attr),
        .config = 0x11,
        .read_format = PERF_FORMAT_TIMING,
        .flags = PERF_ATTR_DISABLED,
    };
    long fd = perf_open(&attr, child, -1);
    if (fd < 0 || ioctl((int)fd, PERF_IOC_ENABLE, 0) != 0) {
        printf("smp-migrate FAILED: open/enable errno=%d\n", errno);
        return 1;
    }
    if (write(gate[1], "x", 1) != 1) {
        return 1;
    }
    close(gate[1]);
    int status = 0;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        printf("smp-migrate FAILED: child status=%d\n", status);
        return 1;
    }
    if (ioctl((int)fd, PERF_IOC_DISABLE, 0) != 0) {
        return 1;
    }
    uint64_t values[3] = {0};
    ssize_t bytes = read((int)fd, values, sizeof(values));
    close((int)fd);
    printf("STARRY_SMP_MIGRATE value=%llu enabled=%llu running=%llu n=%lld\n",
           (unsigned long long)values[0], (unsigned long long)values[1],
           (unsigned long long)values[2], (long long)bytes);
    if (bytes != (ssize_t)sizeof(values) || values[0] == 0 || values[1] == 0 ||
        values[2] == 0 || values[2] > values[1]) {
        puts("smp-migrate FAILED: invalid count/timing");
        return 1;
    }
    puts("STARRY_SMP_MIGRATE_OK");
    return 0;
}
