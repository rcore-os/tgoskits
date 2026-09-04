#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

extern char **environ;

#define PERF_TYPE_SOFTWARE 1u
#define PERF_COUNT_SW_CPU_CLOCK 0ull
#define PERF_COUNT_SW_TASK_CLOCK 1ull
#define PERF_COUNT_SW_PAGE_FAULTS 2ull
#define PERF_COUNT_SW_CONTEXT_SWITCHES 3ull
#define PERF_COUNT_SW_CPU_MIGRATIONS 4ull
#define PERF_EVENT_IOC_ENABLE 0x2400u
#define PERF_EVENT_IOC_DISABLE 0x2401u
#define PERF_EVENT_IOC_RESET 0x2403u
#define ATTR_DISABLED (1ull << 0)
#define ATTR_INHERIT (1ull << 1)
#define ATTR_ENABLE_ON_EXEC (1ull << 12)
#define MADV_DONTNEED 4
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
    uint32_t wakeup_events;
    uint32_t bp_type;
    uint64_t config1;
    uint64_t config2;
};

static int open_sw(uint64_t config, uint64_t flags) {
    struct perf_event_attr attr;
    memset(&attr, 0, sizeof(attr));
    attr.type = PERF_TYPE_SOFTWARE;
    attr.size = sizeof(attr);
    attr.config = config;
    attr.flags = flags;
    return (int)syscall(SYS_perf_event_open, &attr, 0, -1, -1, 0ul);
}

static int open_cpu_clock(int cpu) {
    struct perf_event_attr attr;
    memset(&attr, 0, sizeof(attr));
    attr.type = PERF_TYPE_SOFTWARE;
    attr.size = sizeof(attr);
    attr.config = PERF_COUNT_SW_CPU_CLOCK;
    attr.flags = ATTR_DISABLED;
    return (int)syscall(SYS_perf_event_open, &attr, -1, cpu, -1, 0ul);
}

static volatile uint64_t sink;

static void cpu_work(void) {
    for (uint64_t i = 0; i < 6000000ull; ++i) {
        sink += i * 2654435761ull + 1;
    }
}

static void fault_work(size_t pages) {
    size_t length = pages * 4096;
    volatile uint8_t *p = mmap(NULL, length, PROT_READ | PROT_WRITE,
                               MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (p == MAP_FAILED) {
        return;
    }
    for (size_t i = 0; i < pages; ++i) {
        p[i * 4096] = (uint8_t)i;
    }
    (void)madvise((void *)p, length, MADV_DONTNEED);
    for (size_t i = 0; i < pages; ++i) {
        p[i * 4096] = (uint8_t)(i + 1);
    }
    (void)munmap((void *)p, length);
}

static void workload(void) {
    cpu_work();
    fault_work(64);
    for (int i = 0; i < 8; ++i) {
        struct timespec delay = {0, 1000000};
        (void)nanosleep(&delay, NULL);
    }
}

static int read_value(int fd, uint64_t *value) {
    return read(fd, value, sizeof(*value)) == (ssize_t)sizeof(*value) ? 0 : -1;
}

static int test_enable_on_exec(void) {
    pid_t child = fork();
    if (child == 0) {
        int fd = open_sw(PERF_COUNT_SW_TASK_CLOCK,
                         ATTR_DISABLED | ATTR_ENABLE_ON_EXEC);
        uint64_t before = 1;
        cpu_work();
        if (fd < 0 || read_value(fd, &before) || before != 0) {
            _exit(2);
        }
        char text[16];
        snprintf(text, sizeof(text), "%d", fd);
        char *argv[] = {(char *)"/proc/self/exe", (char *)"--exec", text, NULL};
        execve(argv[0], argv, environ);
        _exit(3);
    }
    int status = 0;
    return child < 0 || waitpid(child, &status, 0) < 0 || !WIFEXITED(status) ||
                   WEXITSTATUS(status) != 0
               ? -1
               : 0;
}

static int test_inherit(void) {
    int fd = open_sw(PERF_COUNT_SW_PAGE_FAULTS, ATTR_DISABLED | ATTR_INHERIT);
    if (fd < 0 || ioctl(fd, PERF_EVENT_IOC_ENABLE, 0) != 0) {
        return -1;
    }
    pid_t child = fork();
    if (child == 0) {
        fault_work(96);
        _exit(0);
    }
    int status = 0;
    if (child < 0 || waitpid(child, &status, 0) < 0 || !WIFEXITED(status) ||
        WEXITSTATUS(status) != 0 || ioctl(fd, PERF_EVENT_IOC_DISABLE, 0) != 0) {
        close(fd);
        return -1;
    }
    uint64_t faults = 0;
    int result = read_value(fd, &faults) == 0 && faults >= 64 ? 0 : -1;
    printf("STARRY_PERF_SW_INHERIT faults=%llu\n",
           (unsigned long long)faults);
    close(fd);
    return result;
}

static int test_systemwide(void) {
    int fd = open_cpu_clock(0);
    if (fd < 0 || ioctl(fd, PERF_EVENT_IOC_ENABLE, 0) != 0) {
        return -1;
    }
    struct timespec delay = {0, 2000000};
    (void)nanosleep(&delay, NULL);
    if (ioctl(fd, PERF_EVENT_IOC_DISABLE, 0) != 0) {
        close(fd);
        return -1;
    }
    uint64_t value = 0;
    int result = read_value(fd, &value) == 0 && value > 0 ? 0 : -1;
    printf("STARRY_PERF_SW_SYSTEMWIDE cpu-clock=%llu\n",
           (unsigned long long)value);
    close(fd);
    return result;
}

int main(int argc, char **argv) {
    if (argc == 3 && strcmp(argv[1], "--exec") == 0) {
        int fd = atoi(argv[2]);
        uint64_t after = 0;
        cpu_work();
        return read_value(fd, &after) == 0 && after > 0 ? 0 : 4;
    }

    const uint64_t ids[] = {PERF_COUNT_SW_CPU_CLOCK,
                            PERF_COUNT_SW_TASK_CLOCK,
                            PERF_COUNT_SW_PAGE_FAULTS,
                            PERF_COUNT_SW_CONTEXT_SWITCHES,
                            PERF_COUNT_SW_CPU_MIGRATIONS};
    const char *names[] = {"cpu-clock", "task-clock", "page-faults",
                           "context-switches", "cpu-migrations"};
    int fds[5];
    for (size_t i = 0; i < 5; ++i) {
        fds[i] = open_sw(ids[i], ATTR_DISABLED);
        if (fds[i] < 0 || ioctl(fds[i], PERF_EVENT_IOC_RESET, 0) != 0 ||
            ioctl(fds[i], PERF_EVENT_IOC_ENABLE, 0) != 0) {
            printf("perf-sw-counters FAILED: open/enable %s errno=%d\n", names[i],
                   errno);
            return 1;
        }
    }
    workload();
    for (size_t i = 0; i < 5; ++i) {
        uint64_t value = 0;
        if (ioctl(fds[i], PERF_EVENT_IOC_DISABLE, 0) != 0 ||
            read_value(fds[i], &value) != 0 || (i < 4 && value == 0)) {
            printf("perf-sw-counters FAILED: read %s value=%llu errno=%d\n",
                   names[i], (unsigned long long)value, errno);
            return 1;
        }
        printf("STARRY_PERF_SW %s=%llu\n", names[i],
               (unsigned long long)value);
        close(fds[i]);
    }
    if (test_enable_on_exec() != 0 || test_inherit() != 0 ||
        test_systemwide() != 0) {
        printf("perf-sw-counters FAILED: exec/inherit/systemwide\n");
        return 1;
    }
    printf("STARRY_PERF_SW_COUNTERS_OK\n");
    return 0;
}
