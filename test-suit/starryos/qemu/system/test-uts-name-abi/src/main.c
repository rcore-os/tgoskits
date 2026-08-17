#define _GNU_SOURCE

#include <errno.h>
#include <limits.h>
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

static int check_error(const char *name, long result, int expected_errno)
{
    if (result == -1 && errno == expected_errno) {
        return 0;
    }
    fprintf(stderr, "FAIL: %s returned %ld errno=%d (%s), expected errno=%d\n", name, result,
            errno, strerror(errno), expected_errno);
    return 1;
}

static int child_test(void)
{
#if ULONG_MAX <= UINT32_MAX
    puts("SKIP: this regression requires a syscall argument wider than 32 bits");
    return 0;
#else
    const unsigned long oversized_len = (1UL << 32) | 1UL;

    if (geteuid() != 0) {
        fputs("SKIP: UTS setter length validation requires root\n", stderr);
        return 0;
    }

    errno = 0;
    if (check_error("sethostname oversized length", raw_sethostname(oversized_len), EINVAL) != 0) {
        return 1;
    }
    errno = 0;
    if (check_error("setdomainname oversized length", raw_setdomainname(oversized_len), EINVAL) != 0) {
        return 1;
    }

    if (setuid(1000) != 0) {
        fprintf(stderr, "FAIL: setuid: errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }

    errno = 0;
    if (check_error("sethostname permission check", raw_sethostname(oversized_len), EPERM) != 0) {
        return 1;
    }
    errno = 0;
    if (check_error("setdomainname permission check", raw_setdomainname(oversized_len), EPERM) != 0) {
        return 1;
    }
    return 0;
#endif
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

    puts("PASS: UTS setters preserve the size_t ABI and error order");
    return 0;
}
