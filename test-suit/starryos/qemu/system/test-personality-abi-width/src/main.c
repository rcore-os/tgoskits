#define _GNU_SOURCE

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef __NR_personality
#error "__NR_personality is required by this test"
#endif

static long raw_personality(unsigned long persona)
{
    return syscall(__NR_personality, persona);
}

static int fail(const char *message)
{
    printf("FAIL: %s: errno=%d (%s)\n", message, errno, strerror(errno));
    return 1;
}

int main(void)
{
    const unsigned long query = UINT32_MAX;
    const unsigned long high_word_only = 1UL << 32;

    errno = 0;
    long before = raw_personality(query);
    if (before < 0) {
        return fail("query original personality");
    }

    errno = 0;
    long changed = raw_personality(high_word_only);
    int changed_errno = errno;

    errno = 0;
    long after = raw_personality(query);
    int after_errno = errno;

    errno = 0;
    long restored = raw_personality((unsigned int)before);
    int restore_errno = errno;

    if (changed < 0) {
        errno = changed_errno;
        return fail("set personality with an upper-word-only raw value");
    }
    if (changed != before) {
        printf("FAIL: setting personality must return the previous value, got %ld expected %ld\n",
               changed, before);
        return 1;
    }
    if (after < 0) {
        errno = after_errno;
        return fail("query personality after upper-word-only raw value");
    }
    if (after != 0) {
        printf("FAIL: personality argument is unsigned int; upper-word-only value must become 0, got %ld\n",
               after);
        return 1;
    }
    if (restored < 0) {
        errno = restore_errno;
        return fail("restore original personality");
    }

    puts("PASS: personality preserves the unsigned-int syscall ABI");
    return 0;
}
