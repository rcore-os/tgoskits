#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

static int passed;
static int failed;

static void expect_true(int condition, const char *name)
{
    if (condition) {
        printf("PASS: %s\n", name);
        passed++;
        return;
    }
    printf("FAIL: %s: errno=%d (%s)\n", name, errno, strerror(errno));
    failed++;
}

static ssize_t read_value(const char *path, char *value, size_t capacity)
{
    int fd = (int)syscall(SYS_openat, AT_FDCWD, path, O_RDONLY | O_CLOEXEC, 0);
    if (fd < 0) {
        return -1;
    }
    ssize_t count = syscall(SYS_read, fd, value, capacity);
    int saved_errno = errno;
    syscall(SYS_close, fd);
    errno = saved_errno;
    return count;
}

static void check_writable_limit(const char *path, const char *label)
{
    char original[64] = {0};
    char current[64] = {0};
    ssize_t length = read_value(path, original, sizeof(original));
    expect_true(length > 0, label);
    if (length <= 0) {
        return;
    }

    errno = 0;
    int fd = (int)syscall(SYS_openat, AT_FDCWD, path, O_WRONLY | O_CLOEXEC, 0);
    expect_true(fd >= 0, "open sysctl O_WRONLY");
    if (fd >= 0) {
        errno = 0;
        expect_true(syscall(SYS_write, fd, original, (size_t)length) == length,
                    "write same sysctl value through O_WRONLY");
        syscall(SYS_close, fd);
    }

    errno = 0;
    fd = (int)syscall(SYS_openat, AT_FDCWD, path, O_RDWR | O_CLOEXEC, 0);
    expect_true(fd >= 0, "open sysctl O_RDWR");
    if (fd >= 0) {
        errno = 0;
        expect_true(syscall(SYS_lseek, fd, 0, SEEK_SET) == 0,
                    "seek writable sysctl to start");
        expect_true(syscall(SYS_write, fd, original, (size_t)length) == length,
                    "write same sysctl value through O_RDWR");
        syscall(SYS_close, fd);
    }

    ssize_t current_length = read_value(path, current, sizeof(current));
    expect_true(current_length == length &&
                    memcmp(current, original, (size_t)length) == 0,
                "sysctl write is visible on readback");
}

int main(void)
{
    printf("=== bugfix-proc-sysctl-writable-limits ===\n");
    check_writable_limit("/proc/sys/kernel/pid_max", "read kernel.pid_max");
    check_writable_limit("/proc/sys/vm/max_map_count", "read vm.max_map_count");

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("STARRY_PROC_SYSCTL_WRITABLE_LIMITS_PASSED\n");
        printf("STARRY_GROUPED_TEST_PASSED: bugfix-proc-sysctl-writable-limits\n");
        return EXIT_SUCCESS;
    }
    printf("STARRY_GROUPED_TEST_FAILED: bugfix-proc-sysctl-writable-limits\n");
    return EXIT_FAILURE;
}
