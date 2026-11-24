#define _GNU_SOURCE

#include <errno.h>
#include <grp.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/types.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#if !defined(__NR_getgroups) || !defined(__NR_setgroups)
#error "getgroups and setgroups syscall numbers are required by this test"
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

static int child_main(void)
{
    gid_t group = getgid();
    if (setgroups(1, &group) != 0) {
        if (errno == EPERM) {
            return 77;
        }
        return fail("setgroups(1, [getgid()])");
    }

    errno = 0;
    long expected = raw_getgroups(0);
    if (expected != 1) {
        printf("FAIL: getgroups(0, NULL) after setgroups must return 1, got %ld\n", expected);
        return 1;
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

    return 0;
}

int main(void)
{
    pid_t child = fork();
    if (child < 0) {
        return fail("fork");
    }
    if (child == 0) {
        _exit(child_main());
    }

    int status = 0;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status)) {
        fputs("FAIL: getgroups ABI child did not exit normally\n", stderr);
        return 1;
    }
    if (WEXITSTATUS(status) == 77) {
        puts("SKIP: getgroups ABI width test requires CAP_SETGID");
        return 0;
    }
    if (WEXITSTATUS(status) != 0) {
        fprintf(stderr, "FAIL: getgroups ABI child exited with %d\n", WEXITSTATUS(status));
        return 1;
    }

    puts("PASS: getgroups preserves the signed-int syscall ABI");
    return 0;
}
