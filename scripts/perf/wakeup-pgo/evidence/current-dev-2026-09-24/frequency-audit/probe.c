#define _GNU_SOURCE
#include <errno.h>
#include <inttypes.h>
#include <linux/perf_event.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

struct perf_read {
    uint64_t cycles;
    uint64_t time_enabled_ns;
    uint64_t time_running_ns;
};

static uint64_t monotonic_ns(void)
{
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
        perror("clock_gettime");
        exit(1);
    }
    return (uint64_t)ts.tv_sec * 1000000000ULL + (uint64_t)ts.tv_nsec;
}

static void fail(const char *operation)
{
    perror(operation);
    exit(1);
}

int main(int argc, char **argv)
{
    if (argc != 2) {
        fprintf(stderr, "usage: %s CPU\n", argv[0]);
        return 2;
    }
    char *end;
    long parsed = strtol(argv[1], &end, 10);
    if (*end != '\0' || parsed < 0 || parsed >= CPU_SETSIZE) {
        fprintf(stderr, "invalid CPU: %s\n", argv[1]);
        return 2;
    }
    int cpu = (int)parsed;
    cpu_set_t affinity;
    CPU_ZERO(&affinity);
    CPU_SET(cpu, &affinity);
    if (sched_setaffinity(0, sizeof(affinity), &affinity) != 0) {
        fail("sched_setaffinity");
    }
    if (sched_getcpu() != cpu) {
        fprintf(stderr, "CPU placement mismatch before measurement\n");
        return 1;
    }

    struct perf_event_attr attr = {0};
    attr.type = PERF_TYPE_HARDWARE;
    attr.size = sizeof(attr);
    attr.config = PERF_COUNT_HW_CPU_CYCLES;
    attr.read_format = PERF_FORMAT_TOTAL_TIME_ENABLED | PERF_FORMAT_TOTAL_TIME_RUNNING;
    attr.disabled = 1;
    int fd = (int)syscall(SYS_perf_event_open, &attr, -1, cpu, -1, 0);
    if (fd < 0) {
        fail("perf_event_open");
    }

    volatile uint64_t sink = 1;
    for (int rep = 0; rep < 3; rep++) {
        if (ioctl(fd, PERF_EVENT_IOC_RESET, 0) != 0) {
            fail("PERF_EVENT_IOC_RESET");
        }
        uint64_t start = monotonic_ns();
        if (ioctl(fd, PERF_EVENT_IOC_ENABLE, 0) != 0) {
            fail("PERF_EVENT_IOC_ENABLE");
        }
        uint64_t now;
        do {
            for (unsigned int i = 0; i < 16384; i++) {
                sink = sink * 6364136223846793005ULL + 1442695040888963407ULL;
            }
            now = monotonic_ns();
        } while (now - start < 500000000ULL);
        if (ioctl(fd, PERF_EVENT_IOC_DISABLE, 0) != 0) {
            fail("PERF_EVENT_IOC_DISABLE");
        }
        struct perf_read value;
        if (read(fd, &value, sizeof(value)) != sizeof(value)) {
            fail("read perf event");
        }
        if (sched_getcpu() != cpu || value.cycles == 0 ||
            value.time_running_ns < 400000000ULL ||
            value.time_running_ns > value.time_enabled_ns) {
            fprintf(stderr, "invalid perf read cpu=%d rep=%d cycles=%" PRIu64
                    " enabled=%" PRIu64 " running=%" PRIu64 "\n",
                    cpu, rep, value.cycles, value.time_enabled_ns,
                    value.time_running_ns);
            return 1;
        }
        printf("FREQ_PROBE cpu=%d rep=%d cycles=%" PRIu64
               " enabled_ns=%" PRIu64 " running_ns=%" PRIu64
               " wall_ns=%" PRIu64 " mhz=%.3f sink=%" PRIu64 "\n",
               cpu, rep, value.cycles, value.time_enabled_ns,
               value.time_running_ns, now - start,
               (double)value.cycles * 1000.0 / (double)value.time_running_ns,
               (uint64_t)sink);
        fflush(stdout);
    }
    if (close(fd) != 0) {
        fail("close perf event");
    }
    return 0;
}
