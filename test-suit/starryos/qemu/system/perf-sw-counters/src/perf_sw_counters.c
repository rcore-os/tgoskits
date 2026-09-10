#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
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
#define ATTR_INHERIT_THREAD (1ull << 35)
#define PERF_SAMPLE_IDENTIFIER (1ull << 16)
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

static int open_sw_on_cpu(uint64_t config, uint64_t flags, int cpu) {
    struct perf_event_attr attr;
    memset(&attr, 0, sizeof(attr));
    attr.type = PERF_TYPE_SOFTWARE;
    attr.size = sizeof(attr);
    attr.config = config;
    attr.flags = flags;
    return (int)syscall(SYS_perf_event_open, &attr, 0, cpu, -1, 0ul);
}

static int open_sw(uint64_t config, uint64_t flags) {
    return open_sw_on_cpu(config, flags, -1);
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

static int test_counting_sample_type(void) {
    struct perf_event_attr attr;
    memset(&attr, 0, sizeof(attr));
    attr.type = PERF_TYPE_SOFTWARE;
    attr.size = sizeof(attr);
    attr.config = PERF_COUNT_SW_TASK_CLOCK;
    attr.sample_period = 0;
    attr.sample_type = PERF_SAMPLE_IDENTIFIER;
    attr.flags = ATTR_DISABLED;
    int fd = (int)syscall(SYS_perf_event_open, &attr, 0, -1, -1, 0ul);
    if (fd < 0) {
        return -1;
    }
    close(fd);
    return 0;
}

static int test_inherited_control(void) {
    int command[2], reply[2];
    if (pipe(command) || pipe(reply)) return 1;
    int fd = open_sw(PERF_COUNT_SW_PAGE_FAULTS, ATTR_INHERIT);
    if (fd < 0) return 1;
    pid_t child = fork();
    if (child < 0) return 1;
    if (child == 0) {
        close(command[1]);
        close(reply[0]);
        char byte = 'r';
        if (write(reply[1], &byte, 1) != 1) _exit(2);
        for (int phase = 0; phase < 2; ++phase) {
            if (read(command[0], &byte, 1) != 1) _exit(3);
            volatile char *pages = mmap(NULL, 64 * 4096, PROT_READ | PROT_WRITE,
                                        MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
            if (pages == MAP_FAILED) _exit(4);
            for (int i = 0; i < 64; ++i) pages[i * 4096] = (char)i;
            if (munmap((void *)pages, 64 * 4096) || write(reply[1], &byte, 1) != 1) _exit(5);
        }
        _exit(0);
    }
    close(command[0]);
    close(reply[1]);
    char byte = 0;
    uint64_t before = 0, disabled = 0, enabled = 0;
    int failed = read(reply[0], &byte, 1) != 1 ||
        ioctl(fd, PERF_EVENT_IOC_DISABLE, 0) || read(fd, &before, 8) != 8 ||
        write(command[1], &byte, 1) != 1 || read(reply[0], &byte, 1) != 1 ||
        read(fd, &disabled, 8) != 8;
    failed |= ioctl(fd, PERF_EVENT_IOC_ENABLE, 0) ||
        write(command[1], &byte, 1) != 1 || read(reply[0], &byte, 1) != 1 ||
        ioctl(fd, PERF_EVENT_IOC_DISABLE, 0) || read(fd, &enabled, 8) != 8;
    int status = 0;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status)) failed = 1;
    close(fd);
    close(command[1]);
    close(reply[0]);
    printf("inherit-control before=%llu disabled=%llu enabled=%llu\n",
           (unsigned long long)before, (unsigned long long)disabled,
           (unsigned long long)enabled);
    return failed || disabled != before || enabled <= disabled;
}

static int test_fault_mode_filter(void) {
    /* Both filters together must exclude every fault, regardless of whether
     * it came from an EL0 instruction or a faulting kernel user copy. */
    int fd = open_sw(PERF_COUNT_SW_PAGE_FAULTS, (1ull << 4) | (1ull << 5));
    if (fd < 0) return 1;
    volatile char *pages = mmap(NULL, 32 * 4096, PROT_READ | PROT_WRITE,
                                MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (pages == MAP_FAILED) return 1;
    for (int i = 0; i < 32; ++i) pages[i * 4096] = 1;
    uint64_t value = 1;
    int failed = ioctl(fd, PERF_EVENT_IOC_DISABLE, 0) || read(fd, &value, 8) != 8;
    close(fd);
    munmap((void *)pages, 32 * 4096);
    printf("fault-mode-filter value=%llu\n", (unsigned long long)value);
    return failed || value != 0;
}

static int remote_cpu, remote_tid, remote_phase, remote_error;

static void *remote_clock_work(void *unused) {
    (void)unused;
    cpu_set_t affinity;
    CPU_ZERO(&affinity);
    CPU_SET(remote_cpu, &affinity);
    struct sched_param priority = {.sched_priority = 20};
    if (sched_setaffinity(0, sizeof(affinity), &affinity) ||
        syscall(SYS_sched_setscheduler, 0, SCHED_FIFO, &priority)) {
        __atomic_store_n(&remote_error, errno, __ATOMIC_RELEASE);
        return NULL;
    }
    __atomic_store_n(&remote_tid, (int)syscall(SYS_gettid), __ATOMIC_RELEASE);
    while (__atomic_load_n(&remote_phase, __ATOMIC_ACQUIRE) == 0) {}
    volatile uint64_t work = 0;
    for (uint64_t i = 0; i < 1000000; ++i) work += i;
    __atomic_store_n(&remote_phase, 2, __ATOMIC_RELEASE);
    while (__atomic_load_n(&remote_phase, __ATOMIC_ACQUIRE) == 2) {}
    priority.sched_priority = 0;
    syscall(SYS_sched_setscheduler, 0, SCHED_OTHER, &priority);
    return NULL;
}

static int test_remote_clock_enable(void) {
    cpu_set_t saved, affinity;
    if (sched_getaffinity(0, sizeof(saved), &saved)) return 1;
    int first = -1;
    remote_cpu = -1;
    for (int cpu = 0; cpu < CPU_SETSIZE; ++cpu) {
        if (!CPU_ISSET(cpu, &saved)) continue;
        if (first < 0) first = cpu;
        else { remote_cpu = cpu; break; }
    }
    if (remote_cpu < 0) {
        puts("remote-clock SKIP: requires two allowed CPUs");
        return 0;
    }
    CPU_ZERO(&affinity);
    CPU_SET(first, &affinity);
    if (sched_setaffinity(0, sizeof(affinity), &affinity)) return 1;
    pthread_t thread;
    if (pthread_create(&thread, NULL, remote_clock_work, NULL)) return 1;
    while (!__atomic_load_n(&remote_tid, __ATOMIC_ACQUIRE) &&
           !__atomic_load_n(&remote_error, __ATOMIC_ACQUIRE)) sched_yield();
    struct perf_event_attr attr = {.type = PERF_TYPE_SOFTWARE, .size = sizeof(attr),
        .config = PERF_COUNT_SW_TASK_CLOCK, .flags = ATTR_DISABLED};
    int fd = remote_error ? -1 : (int)syscall(SYS_perf_event_open, &attr, remote_tid, -1, -1, 0ul);
    int open_error = fd < 0 ? errno : 0;
    int failed = fd < 0 || ioctl(fd, PERF_EVENT_IOC_ENABLE, 0);
    __atomic_store_n(&remote_phase, 1, __ATOMIC_RELEASE);
    while (!remote_error && __atomic_load_n(&remote_phase, __ATOMIC_ACQUIRE) != 2) sched_yield();
    uint64_t value = 0;
    if (fd >= 0 && (read(fd, &value, 8) != 8 || ioctl(fd, PERF_EVENT_IOC_DISABLE, 0))) failed = 1;
    __atomic_store_n(&remote_phase, 3, __ATOMIC_RELEASE);
    pthread_join(thread, NULL);
    if (fd >= 0) close(fd);
    if (sched_setaffinity(0, sizeof(saved), &saved)) failed = 1;
    printf("remote-clock value=%llu setup_error=%d open_error=%d\n",
           (unsigned long long)value, remote_error, open_error);
    return failed || value == 0;
}

static volatile uint64_t sink;

static int test_inherited_live_clock(uint64_t kind, int detach_leader) {
    cpu_set_t saved, affinity;
    if (sched_getaffinity(0, sizeof(saved), &saved)) return 1;
    int first = -1;
    remote_cpu = -1;
    for (int cpu = 0; cpu < CPU_SETSIZE; ++cpu) {
        if (!CPU_ISSET(cpu, &saved)) continue;
        if (first < 0) first = cpu;
        else { remote_cpu = cpu; break; }
    }
    if (remote_cpu < 0) {
        puts("inherit-live-clock SKIP: requires two allowed CPUs");
        return 0;
    }
    CPU_ZERO(&affinity);
    CPU_SET(first, &affinity);
    if (sched_setaffinity(0, sizeof(affinity), &affinity)) return 1;
    remote_tid = remote_phase = remote_error = 0;
    struct perf_event_attr attr = {.type = PERF_TYPE_SOFTWARE, .size = sizeof(attr),
        .config = kind, .flags = ATTR_DISABLED | ATTR_INHERIT,
        .read_format = 3}; /* value, time_enabled, time_running */
    /* Only the inherited binding can run on this CPU. Enable after its FIFO
     * handshake, so no completed child slice can hide a missing live read. */
    int leader = -1;
    if (detach_leader) {
        leader = (int)syscall(SYS_perf_event_open, &attr, 0, remote_cpu, -1, 0ul);
        if (leader < 0) {
            sched_setaffinity(0, sizeof(saved), &saved);
            return 1;
        }
        attr.flags = ATTR_INHERIT; /* Enabled sibling, gated by its leader. */
    }
    int fd = (int)syscall(SYS_perf_event_open, &attr, 0, remote_cpu, leader, 0ul);
    pthread_t thread;
    if (fd < 0 || pthread_create(&thread, NULL, remote_clock_work, NULL)) {
        if (fd >= 0) close(fd);
        if (leader >= 0) close(leader);
        sched_setaffinity(0, sizeof(saved), &saved);
        return 1;
    }
    while (!__atomic_load_n(&remote_tid, __ATOMIC_ACQUIRE) &&
           !__atomic_load_n(&remote_error, __ATOMIC_ACQUIRE)) sched_yield();
    uint64_t before[3] = {0}, live[3] = {0}, stopped[3] = {0}, again[3] = {0};
    int failed = remote_error;
    if (detach_leader) {
        /* Closing the shared last leader FD must resume both the root sibling
         * and its inherited binding while the FIFO child is still running. */
        if (read(fd, before, sizeof(before)) != sizeof(before) ||
            before[0] || before[1] || before[2]) failed = 1;
        if (close(leader)) failed = 1;
    } else if (ioctl(fd, PERF_EVENT_IOC_ENABLE, 0)) {
        failed = 1;
    }
    if (read(fd, before, sizeof(before)) != sizeof(before)) failed = 1;
    __atomic_store_n(&remote_phase, 1, __ATOMIC_RELEASE);
    while (!remote_error && __atomic_load_n(&remote_phase, __ATOMIC_ACQUIRE) != 2)
        sched_yield();
    struct timespec read_start = {0}, stop_end = {0};
    if (clock_gettime(CLOCK_MONOTONIC, &read_start)) failed = 1;
    if (read(fd, live, sizeof(live)) != sizeof(live) ||
        live[0] <= before[0] || live[0] != live[2] || live[1] < live[2]) failed = 1;
    if (ioctl(fd, PERF_EVENT_IOC_DISABLE, 0) ||
        read(fd, stopped, sizeof(stopped)) != sizeof(stopped) ||
        read(fd, again, sizeof(again)) != sizeof(again) ||
        memcmp(stopped, again, sizeof(stopped)) || stopped[0] < live[0] ||
        stopped[0] != stopped[2]) failed = 1;
    if (clock_gettime(CLOCK_MONOTONIC, &stop_end)) failed = 1;
    uint64_t elapsed = (stop_end.tv_sec - read_start.tv_sec) * 1000000000ll +
                       stop_end.tv_nsec - read_start.tv_nsec;
    /* One CPU can add at most one elapsed interval of runtime. Exactly two
     * enabled bindings can add at most two intervals of time_enabled. This
     * catches both recounting a live slice and omitting the child's live
     * enabled window, without a host-speed-dependent performance threshold. */
    if (stopped[0] - live[0] > elapsed || stopped[1] < live[1] ||
        stopped[1] - live[1] > 2 * elapsed) failed = 1;
    if (ioctl(fd, PERF_EVENT_IOC_RESET, 0) ||
        read(fd, again, sizeof(again)) != sizeof(again) || again[0] != 0 ||
        again[1] != stopped[1] || again[2] != stopped[2]) failed = 1;
    __atomic_store_n(&remote_phase, 3, __ATOMIC_RELEASE);
    if (pthread_join(thread, NULL)) failed = 1;
    printf("inherit-live-clock kind=%llu detach=%d before=%llu live=%llu/%llu/%llu stopped=%llu setup_error=%d\n",
           (unsigned long long)kind, detach_leader, (unsigned long long)before[0],
           (unsigned long long)live[0], (unsigned long long)live[1],
           (unsigned long long)live[2], (unsigned long long)stopped[0], remote_error);
    close(fd);
    if (sched_setaffinity(0, sizeof(saved), &saved)) failed = 1;
    return failed;
}

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

static int test_inherited_exec(void) {
#if defined(__aarch64__)
    /* Two preferred-cycle parents exercise dedicated and fallback resources.
     * Only their inherited children enable at exec, so parent reads contain
     * exclusively child work and both physical reservations must be distinct. */
    struct perf_event_attr attr = {
        .type = 0,
        .size = sizeof(attr),
        .config = 0,
        .flags = ATTR_DISABLED | ATTR_INHERIT | ATTR_ENABLE_ON_EXEC,
    };
    int fds[2];
    for (int i = 0; i < 2; ++i) {
        fds[i] = (int)syscall(SYS_perf_event_open, &attr, 0, -1, -1, 0ul);
        if (fds[i] < 0) return -1;
    }
    pid_t child = fork();
    if (child == 0) {
        char *args[] = {(char *)"/proc/self/exe", (char *)"--child-work", NULL};
        execve(args[0], args, environ);
        _exit(7);
    }
    int status = 0;
    if (child < 0 || waitpid(child, &status, 0) != child ||
        !WIFEXITED(status) || WEXITSTATUS(status) != 0) return -1;
    for (int i = 0; i < 2; ++i) {
        uint64_t value = 0;
        if (read_value(fds[i], &value) != 0 || value == 0) {
            puts("perf-sw-counters FAILED: inherited hardware exec/counting");
            return -1;
        }
        close(fds[i]);
    }
#endif
    return 0;
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

static int test_inherit_thread_excludes_fork(void) {
    int fd = open_sw(PERF_COUNT_SW_TASK_CLOCK, ATTR_DISABLED | ATTR_INHERIT |
                     ATTR_INHERIT_THREAD | ATTR_ENABLE_ON_EXEC);
    if (fd < 0) return -1;
    pid_t child = fork();
    if (child == 0) {
        char *args[] = {(char *)"/proc/self/exe", (char *)"--child-work", NULL};
        execve(args[0], args, environ);
        _exit(1);
    }
    int status = 0;
    uint64_t value = UINT64_MAX;
    int failed = child < 0 || waitpid(child, &status, 0) != child ||
        !WIFEXITED(status) || WEXITSTATUS(status) != 0 ||
        read_value(fd, &value) != 0 || value != 0;
    printf("STARRY_PERF_INHERIT_THREAD fork-count=%llu\n", (unsigned long long)value);
    close(fd);
    return failed ? -1 : 0;
}

static void *inherited_thread_work(void *unused) {
    (void)unused;
    cpu_set_t mask;
    CPU_ZERO(&mask);
    CPU_SET(1, &mask);
    if (sched_setaffinity(0, sizeof(mask), &mask) != 0) return (void *)1;
    cpu_work();
    return NULL;
}

static int test_inherit_thread_includes_thread(void) {
    if (sysconf(_SC_NPROCESSORS_ONLN) < 2) {
        puts("STARRY_PERF_INHERIT_THREAD thread-positive skipped: requires SMP");
        return 0;
    }
    cpu_set_t saved, mask;
    if (sched_getaffinity(0, sizeof(saved), &saved) != 0) return -1;
    CPU_ZERO(&mask);
    CPU_SET(0, &mask);
    if (sched_setaffinity(0, sizeof(mask), &mask) != 0) return -1;
    /* The parent stays on CPU0 and cannot contribute to this CPU1-filtered
     * event. Only the inherited thread can make the final count nonzero. */
    int fd = open_sw_on_cpu(PERF_COUNT_SW_TASK_CLOCK,
                            ATTR_INHERIT | ATTR_INHERIT_THREAD, 1);
    pthread_t thread;
    void *result = (void *)1;
    int failed = fd < 0 || pthread_create(&thread, NULL, inherited_thread_work, NULL) != 0;
    if (!failed) failed = pthread_join(thread, &result) != 0 || result != NULL;
    uint64_t value = 0;
    if (fd >= 0) {
        if (read_value(fd, &value) != 0 || value == 0) failed = 1;
        close(fd);
    }
    if (sched_setaffinity(0, sizeof(saved), &saved) != 0) failed = 1;
    printf("STARRY_PERF_INHERIT_THREAD thread-count=%llu\n", (unsigned long long)value);
    return failed ? -1 : 0;
}

static int test_stopped_task_clock(void) {
    pid_t child = fork();
    if (child < 0) return 1;
    if (!child) {
        raise(SIGSTOP);
        cpu_work();
        _exit(0);
    }
    int status;
    if (waitpid(child, &status, WUNTRACED) != child || !WIFSTOPPED(status))
        return 1;
    struct perf_event_attr attr = {
        .type = PERF_TYPE_SOFTWARE, .size = sizeof(attr),
        .config = PERF_COUNT_SW_TASK_CLOCK, .read_format = 3,
    };
    int fd = (int)syscall(SYS_perf_event_open, &attr, child, -1, -1, 0ul);
    if (fd < 0) return 1;
    uint64_t stopped[3] = {0}, done[3] = {0};
    int failed = read(fd, stopped, sizeof(stopped)) != sizeof(stopped) ||
        stopped[0] != 0 || stopped[1] != 0 || stopped[2] != 0;
    if (kill(child, SIGCONT) || waitpid(child, &status, 0) != child ||
        !WIFEXITED(status) || WEXITSTATUS(status) ||
        read(fd, done, sizeof(done)) != sizeof(done)) failed = 1;
    /* Without multiplexing or a CPU filter, both context times are runtime. */
    if (done[0] == 0 || done[1] != done[2] || done[0] != done[2]) failed = 1;
    printf("stopped-task-clock stopped=%llu/%llu/%llu done=%llu/%llu/%llu failed=%d\n",
           (unsigned long long)stopped[0], (unsigned long long)stopped[1],
           (unsigned long long)stopped[2], (unsigned long long)done[0],
           (unsigned long long)done[1], (unsigned long long)done[2], failed);
    close(fd);
    return failed;
}

int main(int argc, char **argv) {
    if (argc == 2 && strcmp(argv[1], "--child-work") == 0) {
        workload();
        return 0;
    }
    if (argc == 3 && strcmp(argv[1], "--exec") == 0) {
        int fd = atoi(argv[2]);
        uint64_t after = 0;
        cpu_work();
        if (read_value(fd, &after) != 0 || after == 0 ||
            ioctl(fd, PERF_EVENT_IOC_DISABLE, 0) != 0 ||
            ioctl(fd, PERF_EVENT_IOC_RESET, 0) != 0) return 4;
        char *next[] = {argv[0], (char *)"--exec-again", argv[2], NULL};
        execve("/proc/self/exe", next, environ);
        return 5;
    }
    if (argc == 3 && strcmp(argv[1], "--exec-again") == 0) {
        uint64_t after = 1;
        cpu_work();
        if (read_value(atoi(argv[2]), &after) != 0 || after != 0) {
            puts("perf-sw-counters FAILED: enable_on_exec was not consumed");
            return 6;
        }
        return 0;
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
    if (test_counting_sample_type() != 0 || test_enable_on_exec() != 0 ||
        test_inherit() != 0 || test_inherited_exec() != 0 ||
        test_systemwide() != 0 || test_inherit_thread_excludes_fork() != 0 ||
        test_inherit_thread_includes_thread() != 0) {
        printf("perf-sw-counters FAILED: exec/inherit/systemwide\n");
        return 1;
    }
    int review_failures = test_inherited_control();
    review_failures += test_fault_mode_filter();
    review_failures += test_remote_clock_enable();
    review_failures += test_stopped_task_clock();
    review_failures += test_inherited_live_clock(PERF_COUNT_SW_TASK_CLOCK, 0);
    review_failures += test_inherited_live_clock(PERF_COUNT_SW_CPU_CLOCK, 0);
    review_failures += test_inherited_live_clock(PERF_COUNT_SW_TASK_CLOCK, 1);
    review_failures += test_inherited_live_clock(PERF_COUNT_SW_CPU_CLOCK, 1);
    if (review_failures) {
        puts("perf-sw-counters FAILED: inherit-control/fault-filter/remote-clock/inherit-live-clock");
        return 1;
    }
    printf("STARRY_PERF_SW_COUNTERS_OK\n");
    return 0;
}
