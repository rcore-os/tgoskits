#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

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

static long getcwd_raw(char *buf, size_t size)
{
    return syscall(SYS_getcwd, buf, size);
}

static void expect_raw_getcwd_length(void)
{
    if (chdir("/tmp") != 0) {
        note_fail("chdir /tmp", strerror(errno));
        return;
    }

    char buf[128];
    memset(buf, 0xa5, sizeof(buf));
    errno = 0;
    long ret = getcwd_raw(buf, sizeof(buf));
    long expected = (long)strlen("/tmp") + 1;
    if (ret == expected && strcmp(buf, "/tmp") == 0) {
        note_pass("raw getcwd returns byte length including nul");
        return;
    }

    char detail[192];
    snprintf(detail, sizeof(detail),
             "ret=%ld errno=%d (%s) buf='%s', expected ret=%ld buf='/tmp'",
             ret, errno, strerror(errno), buf, expected);
    note_fail("raw getcwd length", detail);
}

static void expect_small_buffer_erange(void)
{
    char buf[2];
    errno = 0;
    long ret = getcwd_raw(buf, sizeof(buf));
    int saved_errno = errno;
    if (ret == -1 && saved_errno == ERANGE) {
        note_pass("raw getcwd rejects too-small buffer with ERANGE");
        return;
    }

    char detail[160];
    snprintf(detail, sizeof(detail),
             "ret=%ld errno=%d (%s), expected -1/ERANGE",
             ret, saved_errno, strerror(saved_errno));
    note_fail("raw getcwd small buffer", detail);
}

static void expect_null_small_buffer_erange(void)
{
    errno = 0;
    long ret = getcwd_raw(NULL, strlen("/tmp"));
    int saved_errno = errno;
    if (ret == -1 && saved_errno == ERANGE) {
        note_pass("raw getcwd rejects null too-small buffer with ERANGE");
        return;
    }

    char detail[160];
    snprintf(detail, sizeof(detail),
             "ret=%ld errno=%d (%s), expected -1/ERANGE",
             ret, saved_errno, strerror(saved_errno));
    note_fail("raw getcwd null small buffer", detail);
}

static void expect_null_buffer_efault(void)
{
    errno = 0;
    long ret = getcwd_raw(NULL, 32);
    int saved_errno = errno;
    if (ret == -1 && saved_errno == EFAULT) {
        note_pass("raw getcwd rejects null buffer with EFAULT");
        return;
    }

    char detail[160];
    snprintf(detail, sizeof(detail),
             "ret=%ld errno=%d (%s), expected -1/EFAULT",
             ret, saved_errno, strerror(saved_errno));
    note_fail("raw getcwd null buffer", detail);
}

int main(void)
{
    printf("=== bug-getcwd-syscall-return ===\n");

    expect_raw_getcwd_length();
    expect_small_buffer_erange();
    expect_null_small_buffer_erange();
    expect_null_buffer_efault();

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("ALL TESTS PASSED\n");
        return 0;
    }
    printf("SOME TESTS FAILED\n");
    return 1;
}
