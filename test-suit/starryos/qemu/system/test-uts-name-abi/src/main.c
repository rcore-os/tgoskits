#define _GNU_SOURCE

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef __NR_sethostname
#error "__NR_sethostname is required by this test"
#endif
#ifndef __NR_setdomainname
#error "__NR_setdomainname is required by this test"
#endif

static long raw_sethostname(unsigned long len)
{
    return syscall(__NR_sethostname, NULL, len);
}

static long raw_setdomainname(unsigned long len)
{
    return syscall(__NR_setdomainname, NULL, len);
}

static int check_permission_error(const char *name, long result)
{
    if (result == -1 && errno == EPERM) {
        return 0;
    }
    fprintf(stderr, "FAIL: %s with an upper-word-only length returned %ld errno=%d (%s)\n", name,
            result, errno, strerror(errno));
    return 1;
}

static int child_test(void)
{
    if (geteuid() == 0 && setuid(1000) != 0) {
        fprintf(stderr, "FAIL: setuid: errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }
    if (geteuid() == 0) {
        fputs("FAIL: could not enter a nonprivileged credential state\n", stderr);
        return 1;
    }

    errno = 0;
    if (check_permission_error("sethostname", raw_sethostname(1UL << 32)) != 0) {
        return 1;
    }
    errno = 0;
    if (check_permission_error("setdomainname", raw_setdomainname(1UL << 32)) != 0) {
        return 1;
    }
    return 0;
}

int main(void)
{
    pid_t child = fork();
    if (child < 0) {
        fprintf(stderr, "FAIL: fork: errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }
    if (child == 0) {
        _exit(child_test());
    }

    int status = 0;
    if (waitpid(child, &status, 0) != child) {
        fprintf(stderr, "FAIL: waitpid: errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fprintf(stderr, "FAIL: UTS-name ABI child status=%#x\n", status);
        return 1;
    }

    puts("PASS: UTS setters preserve the signed-int syscall ABI and error order");
    return 0;
}
