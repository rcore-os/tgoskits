#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#if defined(__aarch64__)
struct perf_attr {
    uint32_t type, size;
    uint64_t config, period, sample_type, read_format, flags;
    uint8_t tail[80];
};

static uint64_t now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ull + ts.tv_nsec;
}

static int policy(int kind) {
    struct sched_param param = {.sched_priority = kind == SCHED_FIFO ? 20 : 0};
    return syscall(SYS_sched_setscheduler, 0, kind, &param);
}

static int pin_cpu(int cpu) {
    cpu_set_t cpus;
    CPU_ZERO(&cpus);
    CPU_SET(cpu, &cpus);
    return syscall(SYS_sched_setaffinity, 0, sizeof(cpus), &cpus);
}

/* A lower-priority FIFO task takes over CPU0 when the caller moves to CPU2.
 * Neither PMU worker can run there until the operation has acknowledged. */
static pid_t start_blocker(int gate[2], int stop[2]) {
    if (pipe(gate) != 0 || pipe(stop) != 0)
        return -1;
    pid_t child = fork();
    if (child == 0) {
        struct sched_param param = {.sched_priority = 10};
        char byte;
        if (pin_cpu(0) != 0 ||
            syscall(SYS_sched_setscheduler, 0, SCHED_FIFO, &param) != 0 ||
            write(stop[1], "r", 1) != 1 ||
            read(gate[0], &byte, 1) != 1 ||
            fcntl(stop[0], F_SETFL, O_NONBLOCK) != 0)
            _exit(2);
        ssize_t count;
        do {
            count = read(stop[0], &byte, 1);
        } while (count < 0 && errno == EAGAIN);
        _exit(count == 1 && byte == 's' ? 0 : 2);
    }
    if (child > 0) {
        char ready;
        if (read(stop[0], &ready, 1) != 1 || ready != 'r')
            return -1;
    }
    return child;
}

static int move_caller(int remote, int gate) {
    if (!remote)
        return 0;
    return write(gate, "g", 1) == 1 && pin_cpu(2) == 0 ? 0 : -1;
}

/* Acquire a live slice, not merely a nonzero previously committed count.
 * Once FIFO is installed, the fair slice worker cannot run on this CPU.
 * Increasing time_running across two reads therefore proves an active slice
 * which remains active until the operation under test stops it. Sleeping is
 * only a bounded setup wait; a completed slice never satisfies the gate. */
static int acquire_slice(int fd) {
    uint64_t deadline = now_ns() + 5000000000ull;
    do {
        uint64_t before[3], after[3];
        if (policy(SCHED_FIFO) != 0)
            return -1;
        if (syscall(SYS_read, fd, before, sizeof(before)) != sizeof(before) ||
            syscall(SYS_read, fd, after, sizeof(after)) != sizeof(after))
            return -1;
        if (after[2] > before[2])
            return 0;
        if (policy(SCHED_OTHER) != 0)
            return -1;
        struct timespec pause = {.tv_nsec = 1000000};
        nanosleep(&pause, NULL);
    } while (now_ns() < deadline);
    errno = ETIMEDOUT;
    return -1;
}

static int check_stop(unsigned operation, int remote) {
    int gate[2] = {-1, -1}, stop[2] = {-1, -1};
    /* Fork before opening the event so close below really is the last FD. */
    pid_t blocker = remote ? start_blocker(gate, stop) : 0;
    if (blocker < 0)
        return -1;
    struct perf_attr attr = {
        .type = 4, .size = sizeof(attr), .config = 0x11,
        .read_format = 3, .flags = 1,
    };
    int fd = syscall(SYS_perf_event_open, &attr, -1, 0, -1, 0);
    if (fd < 0)
        return -1;
    long page_size = sysconf(_SC_PAGESIZE);
    void *metadata = mmap(NULL, (size_t)page_size, PROT_READ, MAP_SHARED, fd, 0);
    if (metadata == MAP_FAILED)
        return -1;
    if (syscall(SYS_ioctl, fd, 0x2400, 0) != 0 ||
        acquire_slice(fd) != 0)
        return -1;

    /* CPU0 must not run a fair task after the gate. The remote case hands
     * CPU0 to the FIFO blocker before issuing the control operation. */
    if (operation == 0) {
        if (move_caller(remote, gate[1]) != 0 || syscall(SYS_close, fd) != 0 ||
            pin_cpu(0) != 0)
            return -1;
    } else {
        uint64_t before[3], first[3], second[3];
        uint64_t end = now_ns() + 1000000ull;
        while (now_ns() < end) {}
        if (syscall(SYS_read, fd, before, sizeof(before)) != sizeof(before))
            return -1;
        if (move_caller(remote, gate[1]) != 0 ||
            (operation != 0x10000 && syscall(SYS_ioctl, fd, operation, 0) != 0))
            return -1;
        if (remote && operation == 0x2403) {
            /* The caller can be delayed after RESET while the owner keeps
             * counting. Establish that legal state without an arbitrary sleep;
             * comparing the eventual read with the pre-RESET value is invalid. */
            uint64_t delayed[3];
            do {
                if (syscall(SYS_read, fd, delayed, sizeof(delayed)) != sizeof(delayed))
                    return -1;
            } while (delayed[0] <= before[0]);
        }
        if (syscall(SYS_read, fd, first, sizeof(first)) != sizeof(first) ||
            syscall(SYS_read, fd, second, sizeof(second)) != sizeof(second))
            return -1;
        if (operation == 0x2401) {
            if (first[0] == 0 || first[2] == 0 ||
                memcmp(first, second, sizeof(first)) != 0)
                return -1;
            /* DISABLE has synchronously committed the logical event's value
             * and times. Its mmap page must not describe a placeholder slot. */
            volatile uint64_t *fields = (volatile uint64_t *)((char *)metadata + 16);
            if (fields[0] != first[0] || fields[1] != first[1] || fields[2] != first[2]) {
                printf("metadata mismatch offset=%llu read=%llu enabled=%llu/%llu running=%llu/%llu\n",
                       (unsigned long long)fields[0], (unsigned long long)first[0],
                       (unsigned long long)fields[1], (unsigned long long)first[1],
                       (unsigned long long)fields[2], (unsigned long long)first[2]);
                return -1;
            }
        } else if (operation == 0x10000) {
            if (first[0] <= before[0] || second[0] <= first[0] ||
                first[2] < before[2] || second[2] <= first[2])
                return -1;
        } else if (second[0] <= first[0] ||
                   first[1] < before[1] || first[2] < before[2] ||
                   second[2] <= first[2]) {
            printf("FIFO reset before=%llu/%llu/%llu first=%llu/%llu/%llu second=%llu/%llu/%llu\n",
                   (unsigned long long)before[0], (unsigned long long)before[1],
                   (unsigned long long)before[2], (unsigned long long)first[0],
                   (unsigned long long)first[1], (unsigned long long)first[2],
                   (unsigned long long)second[0], (unsigned long long)second[1],
                   (unsigned long long)second[2]);
            return -1;
        }
        /* Restore the owner-CPU setup for the next operation, only after
         * both reads have exercised the remote FIFO-busy path. */
        if (pin_cpu(0) != 0 || syscall(SYS_close, fd) != 0)
            return -1;
    }
    if (munmap(metadata, (size_t)page_size) != 0)
        return -1;
    if (remote && write(stop[1], "s", 1) != 1)
        return -1;
    if (policy(SCHED_OTHER) != 0)
        return -1;
    if (remote) {
        int status;
        if (waitpid(blocker, &status, 0) != blocker || !WIFEXITED(status) ||
            WEXITSTATUS(status) != 0)
            return -1;
        close(gate[0]);
        close(gate[1]);
        close(stop[0]);
        close(stop[1]);
    }
    return 0;
}
#endif

static int run_checks(void) {
#if defined(__aarch64__)
    if (pin_cpu(0) != 0)
        return 1;
    const unsigned operations[] = {0x10000, 0x2401, 0x2403, 0};
    for (unsigned i = 0; i < 8; ++i) {
        unsigned operation = operations[i % 4];
        int remote = i >= 4;
        printf("STARRY_FIFO_STOP_BEGIN operation=%#x remote=%d\n", operation, remote);
        fflush(stdout);
        if (check_stop(operation, remote) != 0) {
            int saved_errno = errno;
            policy(SCHED_OTHER);
            printf("FIFO stop FAILED operation=%#x errno=%d\n",
                   operation, saved_errno);
            return 1;
        }
    }
#else
    puts("SKIP: AArch64 PMU FIFO stop regression");
#endif
    puts("STARRY_FIFO_STOP_OK");
    return 0;
}

int main(void) {
#if defined(__aarch64__)
    /* Keep the watchdog off the FIFO CPU: the ordinary suite reaper itself
     * can otherwise be starved by the regression. A timeout is failure only. */
    cpu_set_t cpus;
    CPU_ZERO(&cpus);
    CPU_SET(1, &cpus);
    if (syscall(SYS_sched_setaffinity, 0, sizeof(cpus), &cpus) != 0)
        return 1;
    int done[2], start[2];
    if (pipe(done) != 0 || pipe(start) != 0)
        return 1;
    pid_t child = fork();
    if (child < 0)
        return 1;
    if (child == 0) {
        close(done[0]);
        if (pin_cpu(0) != 0 || write(done[1], "r", 1) != 1)
            _exit(1);
        char go;
        if (read(start[0], &go, 1) != 1)
            _exit(1);
        int result = run_checks();
        fflush(stdout);
        char status = result == 0 ? 'y' : 'n';
        write(done[1], &status, 1);
        _exit(result);
    }
    close(done[1]);
    char ready;
    if (read(done[0], &ready, 1) != 1 || ready != 'r' ||
        policy(SCHED_FIFO) != 0)
        return 1;
    if (fcntl(done[0], F_SETFL, O_NONBLOCK) != 0)
        return 1;
    if (write(start[1], "g", 1) != 1)
        return 1;
    char status = 0;
    uint64_t deadline = now_ns() + 20000000000ull;
    /* Do not depend on a timer worker which the stuck FIFO CPU can starve. */
    while (now_ns() < deadline) {
        ssize_t count = read(done[0], &status, 1);
        if (count == 1 || count == 0 || errno != EAGAIN)
            break;
    }
    policy(SCHED_OTHER);
    if (status != 'y') {
        puts("STARRY_GROUPED_TEST_FAILED: FIFO PMU stop did not complete");
        fflush(stdout);
        return 1;
    }
    int child_status;
    return waitpid(child, &child_status, 0) == child &&
           WIFEXITED(child_status) && WEXITSTATUS(child_status) == 0 ? 0 : 1;
#else
    return run_checks();
#endif
}
