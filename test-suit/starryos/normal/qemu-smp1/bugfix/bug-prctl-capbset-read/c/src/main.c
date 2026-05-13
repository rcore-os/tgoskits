#define _GNU_SOURCE
#include <errno.h>
#include <limits.h>
#include <stdio.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef PR_CAPBSET_READ
#define PR_CAPBSET_READ 23
#endif

#ifndef CAP_CHOWN
#define CAP_CHOWN 0
#endif

#ifndef CAP_LAST_CAP
#define CAP_LAST_CAP 40
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

static long prctl_raw(int option, unsigned long arg2)
{
    return syscall(SYS_prctl, option, arg2, 0, 0, 0);
}

static void expect_cap_read_valid(void)
{
    errno = 0;
    long ret = prctl_raw(PR_CAPBSET_READ, CAP_CHOWN);
    if (ret == 1) {
        note_pass("PR_CAPBSET_READ reports CAP_CHOWN in bounding set");
        return;
    }

    char detail[160];
    snprintf(detail, sizeof(detail),
             "ret=%ld errno=%d (%s), expected 1",
             ret, errno, strerror(errno));
    note_fail("PR_CAPBSET_READ valid cap", detail);
}

static void expect_cap_read_last_valid(void)
{
    errno = 0;
    long ret = prctl_raw(PR_CAPBSET_READ, CAP_LAST_CAP);
    if (ret == 1) {
        note_pass("PR_CAPBSET_READ accepts CAP_LAST_CAP");
        return;
    }

    char detail[160];
    snprintf(detail, sizeof(detail),
             "ret=%ld errno=%d (%s), expected 1",
             ret, errno, strerror(errno));
    note_fail("PR_CAPBSET_READ CAP_LAST_CAP", detail);
}

static void expect_cap_read_invalid(void)
{
    errno = 0;
    long ret = prctl_raw(PR_CAPBSET_READ, CAP_LAST_CAP + 1);
    int saved_errno = errno;
    if (ret == -1 && saved_errno == EINVAL) {
        note_pass("PR_CAPBSET_READ rejects capability above CAP_LAST_CAP");
        return;
    }

    char detail[160];
    snprintf(detail, sizeof(detail),
             "ret=%ld errno=%d (%s), expected -1/EINVAL",
             ret, saved_errno, strerror(saved_errno));
    note_fail("PR_CAPBSET_READ invalid cap", detail);
}

static void expect_cap_read_high_bits_invalid(void)
{
#if ULONG_MAX > 0xffffffffUL
    unsigned long high_cap = 1UL << 32;
    errno = 0;
    long ret = prctl_raw(PR_CAPBSET_READ, high_cap);
    int saved_errno = errno;
    if (ret == -1 && saved_errno == EINVAL) {
        note_pass("PR_CAPBSET_READ rejects high-bit capability value");
        return;
    }

    char detail[160];
    snprintf(detail, sizeof(detail),
             "ret=%ld errno=%d (%s), expected -1/EINVAL for cap=%lu",
             ret, saved_errno, strerror(saved_errno), high_cap);
    note_fail("PR_CAPBSET_READ high-bit invalid cap", detail);
#else
    note_pass("PR_CAPBSET_READ high-bit capability value skipped on 32-bit long");
#endif
}

int main(void)
{
    printf("=== bug-prctl-capbset-read ===\n");

    expect_cap_read_valid();
    expect_cap_read_last_valid();
    expect_cap_read_invalid();
    expect_cap_read_high_bits_invalid();

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("ALL TESTS PASSED\n");
        return 0;
    }
    printf("SOME TESTS FAILED\n");
    return 1;
}
