#define _GNU_SOURCE

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef __NR_getgroups
#error "__NR_getgroups is required by this test"
#endif

static long raw_getgroups(unsigned long size)
{
    return syscall(__NR_getgroups, size, NULL);
}

static int fail(const char *message)
{
    printf("FAIL: %s: errno=%d (%s)\n", message, errno, strerror(errno));
    return 1;
}

int main(void)
{
    errno = 0;
    long expected = raw_getgroups(0);
    if (expected < 0) {
        return fail("getgroups(0, NULL)");
    }

    errno = 0;
    long high_word = raw_getgroups(1UL << 32);
    if (high_word < 0) {
        return fail("getgroups with an upper-word-only raw size");
    }
    if (high_word != expected) {
        printf("FAIL: upper-word-only getgroups size must narrow to zero, got %ld expected %ld\n",
               high_word, expected);
        return 1;
    }

    errno = 0;
    long negative = raw_getgroups(~0UL);
    if (negative != -1 || errno != EINVAL) {
        printf("FAIL: raw -1 getgroups size must fail with EINVAL, got %ld errno=%d\n", negative,
               errno);
        return 1;
    }

    puts("PASS: getgroups preserves the signed-int syscall ABI");
    return 0;
}
