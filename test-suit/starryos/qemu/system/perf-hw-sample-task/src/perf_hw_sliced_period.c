#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <sched.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#if defined(__aarch64__)
struct attr {
    uint32_t type, size;
    uint64_t config, period, sample_type, read_format, flags;
    uint8_t tail[80];
};
struct meta {
    uint8_t reserved[1024];
    uint64_t head, tail, offset, size;
};

static int check_period(int system_wide) {
    cpu_set_t cpus;
    CPU_ZERO(&cpus);
    CPU_SET(0, &cpus);
    if (sched_setaffinity(0, sizeof(cpus), &cpus)) return 1;
    int go[2], ack[2];
    if (pipe(go) || pipe(ack)) return 1;
    pid_t child = fork();
    if (child < 0) return 1;
    if (!child) {
        char byte;
        close(go[1]); close(ack[0]);
        while (read(go[0], &byte, 1) == 1) {
            if (write(ack[1], &byte, 1) != 1) _exit(2);
        }
        _exit(0);
    }
    close(go[0]); close(ack[1]);
    const uint64_t period = 1000000;
    struct attr attr = {
        .type = 4, .size = sizeof(attr), .config = 0x11,
        .period = period, .sample_type = 1,
        .flags = 1 | (1ull << 5), /* disabled, exclude_kernel */
    };
    int fd = syscall(SYS_perf_event_open, &attr,
                     system_wide ? -1 : 0, system_wide ? 0 : -1, -1, 0);
    if (fd < 0) return 1;
    size_t length = 9 * 4096;
    struct meta *meta = mmap(NULL, length, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (meta == MAP_FAILED || syscall(SYS_ioctl, fd, 0x2400, 0)) return 1;
    uint64_t count = 0, previous = 0, maximum_slice = 0;
    unsigned failed = 0, turns = 0;
    for (; turns < 20000 && count < period * 3; ++turns) {
        volatile uint64_t value = 0;
        for (unsigned i = 0; i < 1000; ++i) value += i;
        (void)value;
        char byte;
        /* A system event remains installed across task switches: explicitly
         * stop it to exercise the same period retention at ENABLE boundaries. */
        if (system_wide && syscall(SYS_ioctl, fd, 0x2401, 0)) { failed = 1; break; }
        /* A same-CPU peer must run between observations. The entire delta
         * bounds both sides of the scheduling boundary, so no individual
         * slice may independently reach one sampling period. */
        if (write(go[1], "g", 1) != 1 || read(ack[0], &byte, 1) != 1 ||
            syscall(SYS_read, fd, &count, sizeof(count)) != sizeof(count)) {
            failed = 1; break;
        }
        uint64_t delta = count - previous;
        if (delta > maximum_slice) maximum_slice = delta;
        if (count < previous || delta >= period) { failed = 1; break; }
        previous = count;
        if (system_wide && syscall(SYS_ioctl, fd, 0x2400, 0)) { failed = 1; break; }
    }
    if (syscall(SYS_ioctl, fd, 0x2401, 0)) failed = 1;
    close(go[1]); close(ack[0]);
    int status;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status))
        failed = 1;
    uint64_t head = __atomic_load_n(&meta->head, __ATOMIC_ACQUIRE);
    if (count < period * 3 || head == 0) failed = 1;
    printf("SLICED_PERIOD system=%d count=%llu period=%llu max_slice=%llu head=%llu turns=%u failed=%u\n",
           system_wide,
           (unsigned long long)count, (unsigned long long)period,
           (unsigned long long)maximum_slice, (unsigned long long)head, turns, failed);
    munmap(meta, length);
    close(fd);
    return failed;
}
#endif

int main(void) {
#if defined(__aarch64__)
    int failed = check_period(0);
    failed |= check_period(1);
    if (failed) {
        puts("STARRY_PERF_SLICED_PERIOD_FAILED");
        return 1;
    }
#else
    puts("SKIP: AArch64 sampling period across task switches");
#endif
    puts("STARRY_PERF_SLICED_PERIOD_OK");
    return 0;
}
