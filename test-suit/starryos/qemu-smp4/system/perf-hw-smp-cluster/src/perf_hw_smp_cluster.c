/*
 * perf-hw-smp-cluster -- big.LITTLE cluster awareness (Layers 4+5), exercised on
 * homogeneous QEMU via the parity test override (write "1" to
 * /proc/sys/kernel/perf_test_force_clusters => even CPU = A55/Little, odd = A76/
 * Big).
 *
 * Checks:
 *  1. Dual sysfs PMUs: /sys/bus/event_source/devices/armv8_cortex_a76/{type,cpus}
 *     report type 10 and the Big CPUs (1,3); armv8_cortex_a55 reports 9 / 0,2.
 *  2. ENOENT: opening the A76 (Big) PMU pinned to a Little CPU (cpu 0) fails with
 *     errno == ENOENT (Linux cpumask gate).
 *  3. Cluster-skip: a per-task event opened against the A76 PMU on a child that
 *     alternates between a Big CPU (counts) and a Little CPU (skipped) yields
 *     value>0 with time_running < time_enabled (perf scales).
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#define A76_TYPE 10u
#define ARM_CPU_CYCLES 0x11ull
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
    union {
        uint64_t sample_period;
        uint64_t sample_freq;
    };
    uint64_t sample_type;
    uint64_t read_format;
    uint64_t flags;
    union {
        uint32_t wakeup_events;
        uint32_t wakeup_watermark;
    };
    uint32_t bp_type;
    union {
        uint64_t bp_addr;
        uint64_t config1;
    };
    union {
        uint64_t bp_len;
        uint64_t config2;
    };
    uint64_t branch_sample_type;
    uint64_t sample_regs_user;
    uint32_t sample_stack_user;
    int32_t clockid;
    uint64_t sample_regs_intr;
    uint32_t aux_watermark;
    uint16_t sample_max_stack;
    uint16_t __reserved_2;
    uint32_t aux_sample_size;
    uint32_t __reserved_3;
};

static long peo(struct perf_event_attr *a, pid_t pid, int cpu, int gfd,
                unsigned long fl) {
    return syscall(SYS_perf_event_open, a, pid, cpu, gfd, fl);
}

/* Read a small file into buf (NUL-terminated, trailing newline stripped). */
static int read_file(const char *path, char *buf, size_t cap) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        return -1;
    }
    ssize_t n = read(fd, buf, cap - 1);
    close(fd);
    if (n < 0) {
        return -1;
    }
    buf[n] = '\0';
    while (n > 0 && (buf[n - 1] == '\n' || buf[n - 1] == '\r')) {
        buf[--n] = '\0';
    }
    return 0;
}

static void child_alternate(void) {
    volatile uint64_t s = 0;
    for (int round = 0; round < 6; round++) {
        cpu_set_t set;
        CPU_ZERO(&set);
        CPU_SET(round % 2 == 0 ? 1 : 0, &set); /* alternate Big(1) and Little(0) */
        (void)sched_setaffinity(0, sizeof(set), &set);
        for (uint64_t k = 0; k < 20000000ull; k++) {
            s += k;
        }
    }
    _exit(0);
}

int main(void) {
    int ok = 1;

    /* Enable the parity cluster override. */
    int pf = open("/proc/sys/kernel/perf_test_force_clusters", O_WRONLY);
    if (pf < 0 || write(pf, "1", 1) != 1) {
        printf("cluster FAILED: cannot enable force-clusters (errno=%d)\n", errno);
        if (pf >= 0) {
            close(pf);
        }
        return 1;
    }
    close(pf);

    /* 1. Dual sysfs PMUs. */
    char buf[64];
    if (read_file("/sys/bus/event_source/devices/armv8_cortex_a76/type", buf,
                  sizeof(buf)) != 0 ||
        strcmp(buf, "10") != 0) {
        printf("cluster FAILED: a76/type = '%s' (want 10)\n", buf);
        ok = 0;
    }
    if (read_file("/sys/bus/event_source/devices/armv8_cortex_a76/cpus", buf,
                  sizeof(buf)) != 0 ||
        strcmp(buf, "1,3") != 0) {
        printf("cluster FAILED: a76/cpus = '%s' (want 1,3)\n", buf);
        ok = 0;
    }
    if (read_file("/sys/bus/event_source/devices/armv8_cortex_a55/type", buf,
                  sizeof(buf)) != 0 ||
        strcmp(buf, "9") != 0) {
        printf("cluster FAILED: a55/type = '%s' (want 9)\n", buf);
        ok = 0;
    }
    if (read_file("/sys/bus/event_source/devices/armv8_cortex_a55/cpus", buf,
                  sizeof(buf)) != 0 ||
        strcmp(buf, "0,2") != 0) {
        printf("cluster FAILED: a55/cpus = '%s' (want 0,2)\n", buf);
        ok = 0;
    }

    /* 2. ENOENT: A76 PMU pinned to a Little CPU (cpu 0). */
    struct perf_event_attr attr;
    for (size_t b = 0; b < sizeof(attr); b++) {
        ((volatile unsigned char *)&attr)[b] = 0;
    }
    attr.type = A76_TYPE;
    attr.config = ARM_CPU_CYCLES;
    attr.size = sizeof(attr);
    attr.flags = PERF_ATTR_DISABLED;
    long bad = peo(&attr, -1, 0, -1, 0); /* cpu 0 = Little, A76 = Big -> ENOENT */
    if (bad >= 0) {
        printf("cluster FAILED: A76 open on Little cpu0 succeeded (want ENOENT)\n");
        close((int)bad);
        ok = 0;
    } else if (errno != ENOENT) {
        printf("cluster FAILED: A76 open on Little cpu0 errno=%d (want %d ENOENT)\n",
               errno, ENOENT);
        ok = 0;
    }

    /* 3. Cluster-skip: per-task A76 event on a child alternating Big/Little. */
    pid_t child = fork();
    if (child == 0) {
        child_alternate();
    }
    if (child < 0) {
        printf("cluster FAILED: fork errno=%d\n", errno);
        return 1;
    }
    for (size_t b = 0; b < sizeof(attr); b++) {
        ((volatile unsigned char *)&attr)[b] = 0;
    }
    attr.type = A76_TYPE;
    attr.config = ARM_CPU_CYCLES;
    attr.size = sizeof(attr);
    attr.read_format = PERF_FORMAT_TIMING;
    attr.flags = PERF_ATTR_DISABLED;
    long fd = peo(&attr, child, -1, -1, 0); /* per-task: follows the task, no ENOENT */
    if (fd < 0) {
        printf("cluster FAILED: per-task A76 open errno=%d\n", errno);
        ok = 0;
    } else {
        (void)ioctl((int)fd, PERF_IOC_ENABLE, 0);
    }

    int st;
    waitpid(child, &st, 0);

    if (fd >= 0) {
        (void)ioctl((int)fd, PERF_IOC_DISABLE, 0);
        uint64_t b3[3] = {0, 0, 0};
        ssize_t n = read((int)fd, b3, sizeof(b3));
        printf("STARRY_SMP_CLUSTER value=%llu enabled=%llu running=%llu n=%lld\n",
               (unsigned long long)b3[0], (unsigned long long)b3[1],
               (unsigned long long)b3[2], (long long)n);
        if (n != 24 || b3[0] == 0) {
            printf("cluster FAILED: A76 per-task event value 0 (Big slices not counted)\n");
            ok = 0;
        }
        if (b3[2] >= b3[1]) {
            printf("cluster FAILED: running>=enabled (Little slices not skipped)\n");
            ok = 0;
        }
        close((int)fd);
    }

    if (ok) {
        printf("STARRY_SMP_CLUSTER_OK\n");
        return 0;
    }
    return 1;
}
