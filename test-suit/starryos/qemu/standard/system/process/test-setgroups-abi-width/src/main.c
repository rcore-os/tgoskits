#define _GNU_SOURCE

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef __NR_getgroups
#error "__NR_getgroups is required by this test"
#endif
#ifndef __NR_setgroups
#error "__NR_setgroups is required by this test"
#endif

static long raw_getgroups(unsigned long size, gid_t *list)
{
    return syscall(__NR_getgroups, size, list);
}

static long raw_setgroups(unsigned long size, const gid_t *list)
{
    return syscall(__NR_setgroups, size, list);
}

static int child_test(void)
{
    gid_t group = getgid();

    errno = 0;
    if (raw_setgroups(1, &group) != 0) {
        fprintf(stderr, "SKIP: setgroups requires CAP_SETGID: errno=%d (%s)\n", errno,
                strerror(errno));
        return 0;
    }

    errno = 0;
    if (raw_setgroups(1UL << 32, NULL) != 0) {
        fprintf(stderr,
                "FAIL: upper-word-only setgroups size must narrow to zero: errno=%d (%s)\n", errno,
                strerror(errno));
        return 1;
    }

    errno = 0;
    long groups = raw_getgroups(0, NULL);
    if (groups != 0) {
        fprintf(stderr, "FAIL: narrowed zero setgroups size must clear groups, got %ld errno=%d\n",
                groups, errno);
        return 1;
    }

    return 0;
}

int main(void)
{
    pid_t child = fork();
    if (child < 0) {
        printf("FAIL: fork: errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }
    if (child == 0) {
        _exit(child_test());
    }

    int status = 0;
    if (waitpid(child, &status, 0) != child) {
        printf("FAIL: waitpid: errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        printf("FAIL: setgroups ABI child status=%#x\n", status);
        return 1;
    }

    puts("PASS: setgroups preserves the signed-int syscall ABI");
    return 0;
}
