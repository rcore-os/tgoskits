#define _GNU_SOURCE

#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/syscall.h>
#include <sys/wait.h>
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

static int child_uid_priority_checks(void)
{
    passed = 0;
    failed = 0;

    long ret = raw_setpriority(PRIO_PROCESS, 0, 10);
    check(ret == 0, "child root can set an initial nice value");

    errno = 0;
    ret = setuid(1000);
    check(ret == 0, "child can drop to uid 1000");

    ret = raw_setpriority(PRIO_PROCESS, 0, 0);
    check(ret == -1 && errno == EPERM,
          "unprivileged child cannot lower nice without CAP_SYS_NICE");

    ret = raw_setpriority(PRIO_USER, 1000, 15);
    check(ret == 0, "setpriority(PRIO_USER, uid, 15) matches uid processes");

    ret = raw_getpriority(PRIO_USER, 1000);
    check(ret == 5, "getpriority(PRIO_USER, uid) reflects uid process nice");

    ret = raw_setpriority(PRIO_USER, 999999, 15);
    check(ret == -1 && errno == ESRCH, "setpriority(PRIO_USER, missing uid) returns ESRCH");

    printf("CHILD RESULT: %d passed / %d failed\n", passed, failed);
    return failed == 0 ? 0 : 1;
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

    pid_t pid = fork();
    if (pid == 0) {
        return child_uid_priority_checks();
    }
    check(pid > 0, "fork child for uid priority checks");
    if (pid > 0) {
        int status = 0;
        ret = waitpid(pid, &status, 0);
        check(ret == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0,
              "child uid priority checks passed");
    }

    printf("RESULT: %d passed / %d failed\n", passed, failed);
    if (failed == 0) {
        printf("TEST PASSED\n");
        return 0;
    }
    return 1;
}
