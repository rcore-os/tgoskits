#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SYS_open_tree
#define SYS_open_tree 428
#endif
#ifndef SYS_fsopen
#define SYS_fsopen 430
#endif
#ifndef SYS_fspick
#define SYS_fspick 433
#endif

static int passed;
static int failed;

static void check(int condition, const char *message)
{
    if (condition) {
        ++passed;
        printf("PASS: %s\n", message);
    } else {
        ++failed;
        printf("FAIL: %s\n", message);
    }
}

static void expect_enosys(const char *name, long ret, int saved_errno)
{
    char message[128];

    snprintf(message, sizeof(message), "%s fails with ENOSYS", name);
    check(ret == -1 && saved_errno == ENOSYS, message);
    if (ret >= 0) {
        printf("INFO: %s unexpectedly returned fd %ld\n", name, ret);
        close((int)ret);
    } else if (saved_errno != ENOSYS) {
        printf("INFO: %s errno: %s\n", name, strerror(saved_errno));
    }
}

int main(void)
{
    long ret;

    errno = 0;
    ret = syscall(SYS_fsopen, "tmpfs", 0);
    expect_enosys("fsopen", ret, errno);

    errno = 0;
    ret = syscall(SYS_fspick, AT_FDCWD, "/", 0);
    expect_enosys("fspick", ret, errno);

    errno = 0;
    ret = syscall(SYS_open_tree, AT_FDCWD, "/", 0);
    expect_enosys("open_tree", ret, errno);

    printf("RESULT: %d passed / %d failed\n", passed, failed);
    if (failed == 0) {
        printf("TEST PASSED\n");
        return 0;
    }
    return 1;
}
