#define _GNU_SOURCE
#include <errno.h>
#include <sched.h>
#include <time.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

#if defined(__aarch64__)
struct perf_attr {
    uint32_t type, size;
    uint64_t config, period, sample_type, read_format, flags;
    uint8_t tail[80];
};

static int check_cpu_slots(int sampling) {
    /* QEMU Cortex-A53 has six programmable counters per CPU. Eight events
     * across four CPUs must fit; no CPU needs more than two physical slots. */
    int fds[8];
    unsigned opened = 0;
    int failed = 0;
    struct perf_attr attr = {
        .type = sampling ? 4u : 0u, .size = sizeof(attr),
        .config = sampling ? 0x11u : 0u,
        .period = sampling ? 1000000u : 0u,
        .sample_type = 1, .read_format = 3, .flags = 1,
    };
    for (unsigned i = 0; i < 8; ++i) {
        int cpu = (int)(i % 4);
        int fd = syscall(SYS_perf_event_open, &attr, -1, cpu, -1, 0);
        if (fd < 0) {
            printf("CPU slot open failed index=%u cpu=%d sampling=%d errno=%d\n",
                   i, cpu, sampling, errno);
            failed = 1;
            break;
        }
        fds[opened++] = fd;
        if (syscall(SYS_ioctl, fd, 0x2400, 0) != 0) {
            failed = 1;
            break;
        }
    }
    /* Flexible counting is scheduled asynchronously. Wait for actual running
     * time before stopping; immediate disable may legitimately precede a slice. */
    struct timespec start, now;
    clock_gettime(CLOCK_MONOTONIC, &start);
    for (unsigned i = 0; i < opened; ++i) {
        uint64_t values[3] = {0};
        while (!failed) {
            if (read(fds[i], values, sizeof(values)) != sizeof(values)) {
                failed = 1;
                break;
            }
            if (values[0] && values[2]) break;
            clock_gettime(CLOCK_MONOTONIC, &now);
            if (now.tv_sec - start.tv_sec >= 10) {
                printf("CPU slot did not run index=%u sampling=%d\n", i, sampling);
                failed = 1;
                break;
            }
            sched_yield();
        }
        if (syscall(SYS_ioctl, fds[i], 0x2401, 0) != 0 ||
            syscall(SYS_read, fds[i], values, sizeof(values)) != sizeof(values) ||
            values[0] == 0 || values[2] == 0)
            failed = 1;
        if (close(fds[i]) != 0)
            failed = 1;
    }
    return failed;
}
#endif

int main(void) {
#if defined(__aarch64__)
    /* Repeat after complete teardown to check per-CPU slot reclamation. */
    if (check_cpu_slots(1) || check_cpu_slots(1) ||
        check_cpu_slots(0) || check_cpu_slots(0)) {
        puts("STARRY_PERF_CPU_SLOTS_FAILED");
        return 1;
    }
#else
    puts("SKIP: AArch64 per-CPU PMU slots");
#endif
    puts("STARRY_PERF_CPU_SLOTS_OK");
    return 0;
}
