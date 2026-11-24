#define _GNU_SOURCE

#include <errno.h>
#include <limits.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

static int failures;

static void expect_errno(long result, int expected, const char *description)
{
    if (result == -1 && errno == expected) {
        printf("PASS: %s\n", description);
        return;
    }

    printf("FAIL: %s: result=%ld errno=%d expected_errno=%d\n",
           description, result, errno, expected);
    failures++;
}

static void expect_result(long result, long expected, const char *description)
{
    if (result == expected) {
        printf("PASS: %s\n", description);
        return;
    }

    printf("FAIL: %s: result=%ld expected=%ld errno=%d\n",
           description, result, expected, errno);
    failures++;
}

int main(void)
{
#if SIZE_MAX <= UINT32_MAX
    puts("SKIP: affinity length truncation requires a 64-bit userspace ABI");
    return 0;
#else
    cpu_set_t original;
    cpu_set_t observed;

    CPU_ZERO(&original);
    errno = 0;
    long mask_bytes = syscall(SYS_sched_getaffinity, 0, sizeof(original), &original);
    if (mask_bytes <= 0 || (size_t)mask_bytes > sizeof(original) ||
        (size_t)mask_bytes % sizeof(unsigned long) != 0) {
        printf("FAIL: read baseline affinity: result=%ld errno=%d\n", mask_bytes, errno);
        return 1;
    }
    puts("PASS: read baseline affinity");

    CPU_ZERO(&observed);
    errno = 0;
    long result = syscall(SYS_sched_getaffinity, 0, (size_t)1 << 28, &observed);
    expect_result(result, mask_bytes,
                  "getaffinity accepts a large aligned length without overflowing");
    if (result == mask_bytes && memcmp(&observed, &original, (size_t)mask_bytes) != 0) {
        puts("FAIL: large-length getaffinity changed the returned mask");
        failures++;
    }

    errno = 0;
    result = syscall(SYS_sched_getaffinity, 0, (size_t)1 << 29, &observed);
    expect_errno(result, EINVAL,
                 "getaffinity uses unsigned-int multiplication overflow semantics");

    errno = 0;
    result = syscall(SYS_sched_getaffinity, 0, (size_t)mask_bytes + 1, &observed);
    expect_errno(result, EINVAL, "getaffinity rejects a non-word-aligned length");

    size_t truncated_length = (size_t)UINT32_MAX + 1;
    errno = 0;
    result = syscall(SYS_sched_getaffinity, 0, truncated_length, &observed);
    expect_errno(result, EINVAL, "getaffinity truncates a 2^32 length to zero");

    errno = 0;
    result = syscall(SYS_sched_setaffinity, 0, (size_t)mask_bytes, &original);
    expect_result(result, 0, "setaffinity accepts the current nonempty mask");

    errno = 0;
    result = syscall(SYS_sched_setaffinity, 0, truncated_length, &original);
    expect_errno(result, EINVAL, "setaffinity truncates a 2^32 length to zero");

    CPU_ZERO(&observed);
    errno = 0;
    result = syscall(SYS_sched_getaffinity, 0, sizeof(observed), &observed);
    expect_result(result, mask_bytes, "read affinity after rejected setaffinity");
    if (result == mask_bytes && memcmp(&observed, &original, (size_t)mask_bytes) != 0) {
        puts("FAIL: rejected setaffinity changed the affinity mask");
        failures++;
    }

    if (failures == 0)
        puts("SCHED_AFFINITY_ABI_PASSED");
    return failures == 0 ? 0 : 1;
#endif
}
