#define _GNU_SOURCE
#include <errno.h>
#include <sched.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SCHED_EXT
#define SCHED_EXT 7
#endif

static int expect_priority(const char *name, long syscall_number)
{
    errno = 0;
    long result = syscall(syscall_number, SCHED_EXT);
    if (result == 0)
        return 0;

    fprintf(stderr, "%s(SCHED_EXT) returned %ld, errno=%d; expected 0\n",
            name, result, errno);
    return 1;
}

int main(void)
{
    int failed = 0;

    failed |= expect_priority("sched_get_priority_max", SYS_sched_get_priority_max);
    failed |= expect_priority("sched_get_priority_min", SYS_sched_get_priority_min);
    if (failed)
        return 1;

    puts("SCHED_EXT_PRIORITY_RANGE_PASSED");
    return 0;
}
