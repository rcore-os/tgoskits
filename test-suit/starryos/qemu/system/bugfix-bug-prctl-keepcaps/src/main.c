#define _GNU_SOURCE
#include <errno.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef PR_SET_KEEPCAPS
#define PR_SET_KEEPCAPS 8
#endif

#ifndef PR_GET_KEEPCAPS
#define PR_GET_KEEPCAPS 7
#endif

#define LINUX_CAPABILITY_VERSION_3 0x20080522U

struct capability_header {
    uint32_t version;
    int32_t pid;
};

struct capability_data {
    uint32_t effective;
    uint32_t permitted;
    uint32_t inheritable;
};

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

static long prctl_raw(int option, unsigned long arg2)
{
    return syscall(SYS_prctl, option, arg2, 0, 0, 0);
}

static int read_capabilities(struct capability_data data[2])
{
    struct capability_header header = {
        .version = LINUX_CAPABILITY_VERSION_3,
        .pid = 0,
    };
    memset(data, 0, sizeof(*data) * 2);
    return (int)syscall(SYS_capget, &header, data);
}

static uint64_t capability_mask(const struct capability_data data[2],
                                int permitted)
{
    uint32_t low = permitted ? data[0].permitted : data[0].effective;
    uint32_t high = permitted ? data[1].permitted : data[1].effective;
    return (uint64_t)low | ((uint64_t)high << 32);
}

static void expect_value(const char *name, long expected)
{
    errno = 0;
    long ret = prctl_raw(PR_GET_KEEPCAPS, 0);
    if (ret == expected) {
        note_pass(name);
        return;
    }

    char detail[160];
    snprintf(detail, sizeof(detail), "ret=%ld errno=%d (%s), expected %ld",
             ret, errno, strerror(errno), expected);
    note_fail(name, detail);
}

static void expect_set(const char *name, unsigned long value)
{
    errno = 0;
    long ret = prctl_raw(PR_SET_KEEPCAPS, value);
    if (ret == 0) {
        note_pass(name);
        return;
    }

    char detail[160];
    snprintf(detail, sizeof(detail), "ret=%ld errno=%d (%s), expected 0",
             ret, errno, strerror(errno));
    note_fail(name, detail);
}

static void expect_invalid_set(unsigned long value, const char *name)
{
    errno = 0;
    long ret = prctl_raw(PR_SET_KEEPCAPS, value);
    int saved_errno = errno;
    if (ret == -1 && saved_errno == EINVAL) {
        note_pass(name);
        return;
    }

    char detail[192];
    snprintf(detail, sizeof(detail),
             "ret=%ld errno=%d (%s), expected -1/EINVAL for state=%lu",
             ret, saved_errno, strerror(saved_errno), value);
    note_fail(name, detail);
}

static int verify_setuid_transition(void)
{
    if (geteuid() != 0) {
        fprintf(stderr, "setuid capability transition requires euid 0\n");
        return 1;
    }

    struct capability_data before[2];
    struct capability_data after[2];
    if (read_capabilities(before) != 0) {
        perror("capget before setuid");
        return 1;
    }
    uint64_t permitted_before = capability_mask(before, 1);
    if (permitted_before == 0) {
        fprintf(stderr, "root process unexpectedly has no permitted capabilities\n");
        return 1;
    }

    if (prctl_raw(PR_SET_KEEPCAPS, 1) != 0) {
        perror("PR_SET_KEEPCAPS before setuid");
        return 1;
    }
    if (setuid(65534) != 0) {
        perror("setuid(65534)");
        return 1;
    }
    if (read_capabilities(after) != 0) {
        perror("capget after setuid");
        return 1;
    }

    uint64_t permitted_after = capability_mask(after, 1);
    uint64_t effective_after = capability_mask(after, 0);
    if (permitted_after != permitted_before) {
        fprintf(stderr,
                "permitted capabilities changed: before=%#llx after=%#llx\n",
                (unsigned long long)permitted_before,
                (unsigned long long)permitted_after);
        return 1;
    }
    if (effective_after != 0) {
        fprintf(stderr, "effective capabilities not cleared: %#llx\n",
                (unsigned long long)effective_after);
        return 1;
    }
    if (prctl_raw(PR_GET_KEEPCAPS, 0) != 1) {
        fprintf(stderr, "keepcaps flag was not retained across setuid\n");
        return 1;
    }
    return 0;
}

static int verify_exec_reset(const char *self_path)
{
    if (prctl_raw(PR_SET_KEEPCAPS, 1) != 0) {
        perror("PR_SET_KEEPCAPS before exec");
        return 1;
    }
    execl(self_path, self_path, "--verify-exec-reset", NULL);
    perror("execl self");
    return 1;
}

static void expect_child_success(const char *name,
                                 int (*child_fn)(const char *),
                                 const char *argument)
{
    fflush(NULL);
    pid_t pid = fork();
    if (pid < 0) {
        note_fail(name, "fork failed");
        return;
    }
    if (pid == 0) {
        _exit(child_fn(argument) == 0 ? 0 : 1);
    }

    int status;
    if (waitpid(pid, &status, 0) == pid && WIFEXITED(status) &&
        WEXITSTATUS(status) == 0) {
        note_pass(name);
        return;
    }

    char detail[96];
    snprintf(detail, sizeof(detail), "child status=%#x", status);
    note_fail(name, detail);
}

static int setuid_child_adapter(const char *unused)
{
    (void)unused;
    return verify_setuid_transition();
}

int main(int argc, char **argv)
{
    if (argc == 2 && strcmp(argv[1], "--verify-exec-reset") == 0) {
        return prctl_raw(PR_GET_KEEPCAPS, 0) == 0 ? 0 : 1;
    }

    printf("=== bug-prctl-keepcaps ===\n");

    expect_value("PR_GET_KEEPCAPS defaults to 0 after exec", 0);
    expect_set("PR_SET_KEEPCAPS enables the flag", 1);
    expect_value("PR_GET_KEEPCAPS observes the enabled flag", 1);
    expect_invalid_set(2, "PR_SET_KEEPCAPS rejects state 2");
    expect_invalid_set(ULONG_MAX,
                       "PR_SET_KEEPCAPS rejects an unsigned high value");
    expect_value("invalid states leave the flag unchanged", 1);
    expect_set("PR_SET_KEEPCAPS clears the flag", 0);
    expect_value("PR_GET_KEEPCAPS observes the cleared flag", 0);

    expect_child_success(
        "keepcaps preserves permitted and clears effective caps across setuid",
        setuid_child_adapter, NULL);
    expect_child_success("exec resets the keepcaps flag", verify_exec_reset,
                         argv[0]);

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("ALL TESTS PASSED\n");
        return 0;
    }
    printf("SOME TESTS FAILED\n");
    return 1;
}
