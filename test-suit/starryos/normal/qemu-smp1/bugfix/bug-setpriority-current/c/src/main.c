#define _GNU_SOURCE

#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/syscall.h>
#include <unistd.h>

static int passed;
static int failed;

static void check(int condition, const char *message)
{
    if (condition) {
        ++passed;
        printf("PASS: %s\n", message);
    } else {
        ++failed;
        printf("FAIL: %s errno=%d (%s)\n", message, errno, strerror(errno));
    }
}

static long raw_getpriority(int which, int who)
{
    errno = 0;
    return syscall(SYS_getpriority, which, who);
}

static long raw_setpriority(int which, int who, int prio)
{
    errno = 0;
    return syscall(SYS_setpriority, which, who, prio);
}

int main(void)
{
    const int target_nice = 19;
    const long expected_raw_priority = 20 - target_nice;

    printf("TEST: bug-setpriority-current\n");

    long ret = raw_getpriority(PRIO_PROCESS, 0);
    check(ret >= 1 && ret <= 40, "current process raw priority is in Linux range");

    ret = raw_setpriority(PRIO_PROCESS, 0, target_nice);
    check(ret == 0, "setpriority(PRIO_PROCESS, 0, 19) succeeds");

    ret = raw_getpriority(PRIO_PROCESS, 0);
    check(ret == expected_raw_priority,
          "getpriority reflects the current process nice value");

    ret = raw_setpriority(999, 0, target_nice);
    check(ret == -1 && errno == EINVAL, "setpriority rejects invalid which");

    ret = raw_setpriority(PRIO_PROCESS, 999999, target_nice);
    check(ret == -1 && errno == ESRCH, "setpriority rejects missing process");

    printf("RESULT: %d passed / %d failed\n", passed, failed);
    if (failed == 0) {
        printf("TEST PASSED\n");
        return 0;
    }
    return 1;
}
