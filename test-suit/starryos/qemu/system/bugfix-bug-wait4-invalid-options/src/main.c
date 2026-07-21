#define _GNU_SOURCE
#include <errno.h>
#include <limits.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef WEXITED
#define WEXITED 0x00000004
#endif
#ifndef WNOWAIT
#define WNOWAIT 0x01000000
#endif

static int passed;
static int failed;

static void note_pass(const char *name)
{
    printf("PASS: %s\n", name);
    passed++;
}

static void note_fail(const char *name, const char *detail)
{
    printf("FAIL: %s: %s\n", name, detail);
    failed++;
}

static pid_t wait4_raw(pid_t pid, int *status, int options, struct rusage *usage)
{
    return (pid_t)syscall(SYS_wait4, pid, status, options, usage);
}

static int cleanup_child(pid_t pid, int expect_exit)
{
    int status = 0;
    errno = 0;
    pid_t ret = wait4_raw(pid, &status, 0, NULL);
    if (ret != pid) {
        printf("  cleanup wait4 ret=%ld errno=%d (%s), expected pid=%ld\n",
               (long)ret, errno, strerror(errno), (long)pid);
        return 0;
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != expect_exit) {
        printf("  cleanup status=0x%x, expected exit=%d\n", status, expect_exit);
        return 0;
    }
    return 1;
}

static void expect_invalid_option(const char *name, int option)
{
    pid_t pid = fork();
    if (pid < 0) {
        note_fail(name, "fork failed");
        return;
    }
    if (pid == 0) {
        _exit(7);
    }

    usleep(50000);

    int status = 0x12345678;
    struct rusage usage;
    memset(&usage, 0x5a, sizeof(usage));
    errno = 0;
    pid_t ret = wait4_raw(pid, &status, option, &usage);
    int saved_errno = errno;

    if (ret == -1 && saved_errno == EINVAL) {
        if (status == 0x12345678 && cleanup_child(pid, 7)) {
            note_pass(name);
        } else {
            note_fail(name, "EINVAL returned but status changed or child was not waitable");
        }
        return;
    }

    char detail[192];
    snprintf(detail, sizeof(detail),
             "wait4(option=0x%x) ret=%ld errno=%d (%s), expected -1/EINVAL",
             option, (long)ret, saved_errno, strerror(saved_errno));
    note_fail(name, detail);

    if (ret == 0 || ret == -1) {
        (void)cleanup_child(pid, 7);
    }
}

static void expect_valid_wait4(void)
{
    pid_t pid = fork();
    if (pid < 0) {
        note_fail("valid wait4", "fork failed");
        return;
    }
    if (pid == 0) {
        _exit(9);
    }

    int status = 0;
    struct rusage usage;
    memset(&usage, 0, sizeof(usage));
    errno = 0;
    pid_t ret = wait4_raw(pid, &status, 0, &usage);
    if (ret == pid && WIFEXITED(status) && WEXITSTATUS(status) == 9) {
        note_pass("valid wait4 still reaps exited child");
    } else {
        char detail[192];
        snprintf(detail, sizeof(detail),
                 "ret=%ld errno=%d (%s) status=0x%x",
                 (long)ret, errno, strerror(errno), status);
        note_fail("valid wait4", detail);
        if (ret != pid) {
            (void)cleanup_child(pid, 9);
        }
    }
}

static void expect_int_min_selector(void)
{
    int status = 0x12345678;
    errno = 0;
    pid_t ret = wait4_raw(INT_MIN, &status, WNOHANG, NULL);
    if (ret == -1 && errno == ESRCH && status == 0x12345678) {
        note_pass("wait4 INT_MIN selector returns ESRCH without changing status");
    } else {
        char detail[192];
        snprintf(detail, sizeof(detail),
                 "ret=%ld errno=%d (%s) status=0x%x, expected -1/ESRCH",
                 (long)ret, errno, strerror(errno), status);
        note_fail("wait4 INT_MIN selector", detail);
    }
}

int main(void)
{
    printf("=== bug-wait4-invalid-options ===\n");

    expect_invalid_option("wait4 rejects waitid-only WEXITED", WEXITED);
    expect_invalid_option("wait4 rejects waitid-only WNOWAIT", WNOWAIT);
    expect_invalid_option("wait4 rejects unknown option bit 0x10", 0x10);
    expect_int_min_selector();
    expect_valid_wait4();

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("ALL TESTS PASSED\n");
        return 0;
    }
    printf("SOME TESTS FAILED\n");
    return 1;
}
