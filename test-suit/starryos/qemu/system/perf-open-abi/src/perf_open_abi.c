/* Linux v7.1 perf_event_open attribute, flag, and target ABI regression. */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SYS_perf_event_open
#define SYS_perf_event_open 241
#endif

#define PERF_TYPE_HARDWARE 0u
#define PERF_COUNT_HW_CPU_CYCLES 0u
#define PERF_ATTR_SIZE_VER0 64u
#define PERF_ATTR_SIZE_VER9 144u

#define PERF_FLAG_FD_CLOEXEC (1ul << 3)
#define PERF_FLAG_PID_CGROUP (1ul << 2)

struct perf_event_attr_v9 {
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
    uint64_t branch_sample_type;
    uint64_t sample_regs_user;
    uint32_t sample_stack_user;
    int32_t clockid;
    uint64_t sample_regs_intr;
    uint32_t aux_watermark;
    uint16_t sample_max_stack;
    uint16_t reserved_2;
    uint32_t aux_sample_size;
    uint32_t aux_action;
    uint64_t sig_data;
    uint64_t config3;
    uint64_t config4;
};

_Static_assert(sizeof(struct perf_event_attr_v9) == PERF_ATTR_SIZE_VER9,
               "perf_event_attr v9 layout mismatch");

#if defined(__aarch64__)
static long perf_open(void *attr, int pid, int cpu, int group_fd,
                      unsigned long flags) {
    return syscall(SYS_perf_event_open, attr, pid, cpu, group_fd, flags);
}

static void init_attr(struct perf_event_attr_v9 *attr, uint32_t size) {
    memset(attr, 0, sizeof(*attr));
    attr->type = PERF_TYPE_HARDWARE;
    attr->size = size;
    attr->config = PERF_COUNT_HW_CPU_CYCLES;
    attr->flags = 1; /* disabled */
}

static int expect_errno(const char *name, void *attr, int pid, int cpu,
                        int group_fd, unsigned long flags, int expected) {
    errno = 0;
    long fd = perf_open(attr, pid, cpu, group_fd, flags);
    if (fd >= 0) {
        close((int)fd);
        printf("%s unexpectedly succeeded\n", name);
        return -1;
    }
    if (errno != expected) {
        printf("%s errno=%d expected=%d\n", name, errno, expected);
        return -1;
    }
    return 0;
}

static int expect_open(const char *name, struct perf_event_attr_v9 *attr,
                       int pid, int cpu, unsigned long flags) {
    errno = 0;
    long fd = perf_open(attr, pid, cpu, -1, flags);
    if (fd < 0) {
        printf("%s failed errno=%d\n", name, errno);
        return -1;
    }
    if ((flags & PERF_FLAG_FD_CLOEXEC) != 0 &&
        (fcntl((int)fd, F_GETFD) & FD_CLOEXEC) == 0) {
        printf("%s did not set FD_CLOEXEC\n", name);
        close((int)fd);
        return -1;
    }
    close((int)fd);
    return 0;
}
#endif

int main(void) {
#if !defined(__aarch64__)
    printf("STARRY_PERF_OPEN_ABI_OK\n");
    return 0;
#else
    struct perf_event_attr_v9 attr;
    unsigned char extended[PERF_ATTR_SIZE_VER9 + 8];
    int failures = 0;
    long online = sysconf(_SC_NPROCESSORS_ONLN);

    init_attr(&attr, PERF_ATTR_SIZE_VER0);
    failures += expect_open("short-attr", &attr, 0, -1, 0) != 0;

    init_attr(&attr, 0);
    failures += expect_open("zero-size-quirk", &attr, 0, -1, 0) != 0;

    memset(extended, 0, sizeof(extended));
    init_attr((struct perf_event_attr_v9 *)extended, sizeof(extended));
    failures += expect_open("zero-extension",
                            (struct perf_event_attr_v9 *)extended, 0, -1, 0) !=
                0;

    extended[PERF_ATTR_SIZE_VER9] = 1;
    failures += expect_errno("nonzero-extension", extended, 0, -1, -1, 0,
                             E2BIG) != 0;
    failures +=
        ((struct perf_event_attr_v9 *)extended)->size != PERF_ATTR_SIZE_VER9;

    init_attr(&attr, PERF_ATTR_SIZE_VER0 - 1);
    failures += expect_errno("too-small", &attr, 0, -1, -1, 0, E2BIG) != 0;
    failures += attr.size != PERF_ATTR_SIZE_VER9;

    init_attr(&attr, 4097);
    failures += expect_errno("too-large", &attr, 0, -1, -1, 0, E2BIG) != 0;
    failures += attr.size != PERF_ATTR_SIZE_VER9;

    failures += expect_errno("unknown-flags-before-pointer", (void *)1, 0, -1,
                             -1, 1ul << 12, EINVAL) != 0;

    init_attr(&attr, PERF_ATTR_SIZE_VER9);
    failures += expect_open("cloexec", &attr, 0, -1,
                            PERF_FLAG_FD_CLOEXEC) != 0;
    failures += expect_open("task-cpu-filter", &attr, 0, 0, 0) != 0;
    failures += expect_open("system-wide-cpu", &attr, -1, 0, 0) != 0;

    failures += expect_errno("missing-pid-before-cpu", &attr, INT_MAX,
                             (int)online, -1, 0, ESRCH) != 0;
    failures += expect_errno("negative-pid", &attr, -2, 0, -1, 0, ESRCH) != 0;
    failures += expect_errno("invalid-task-cpu", &attr, 0, (int)online, -1, 0,
                             EINVAL) != 0;
    failures += expect_errno("invalid-system-tuple", &attr, -1, -1, -1, 0,
                             EINVAL) != 0;

    int ordinary_fd = open("/dev/null", O_RDONLY);
    if (ordinary_fd < 0) {
        printf("open /dev/null failed errno=%d\n", errno);
        return 1;
    }
    failures += expect_errno("bad-group-before-pid", &attr, INT_MAX, 0,
                             ordinary_fd, 0, EBADF) != 0;
    close(ordinary_fd);

    failures += expect_errno("invalid-cgroup-tuple", &attr, -1, -1, -1,
                             PERF_FLAG_PID_CGROUP, EINVAL) != 0;
    failures += expect_errno("unsupported-cgroup", &attr, 0, 0, -1,
                             PERF_FLAG_PID_CGROUP, EOPNOTSUPP) != 0;

    printf("STARRY_PERF_OPEN_ABI failures=%d online=%ld\n", failures, online);
    if (failures == 0) {
        printf("STARRY_PERF_OPEN_ABI_OK\n");
        return 0;
    }
    return 1;
#endif
}
