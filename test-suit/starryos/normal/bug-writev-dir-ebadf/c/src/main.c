/*
 * bug-writev-dir-ebadf: Reproduction test for writev/pwrite on directory errno
 *
 * Bug: StarryOS returns EISDIR(21) instead of EBADF(9) when writev/pwrite
 * is called on a directory fd opened O_RDONLY|O_DIRECTORY.
 *
 * Per Linux kernel (vfs_writev in fs/read_write.c), the FMODE_WRITE check
 * happens first at the VFS layer, returning EBADF before any filesystem
 * operation that could return EISDIR.
 */

#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <stdio.h>
#include <string.h>
#include <errno.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/uio.h>

static int __pass = 0;
static int __fail = 0;

#define CHECK(cond, msg) do { \
    if (cond) { \
        printf("  PASS | %s:%d | %s\n", __FILE__, __LINE__, msg); \
        __pass++; \
    } else { \
        printf("  FAIL | %s:%d | %s | errno=%d (%s)\n", \
               __FILE__, __LINE__, msg, errno, strerror(errno)); \
        __fail++; \
    } \
} while(0)

#define CHECK_ERR(call, exp_errno, msg) do { \
    errno = 0; \
    long _r = (long)(call); \
    if (_r == -1 && errno == (exp_errno)) { \
        printf("  PASS | %s:%d | %s (errno=%d as expected)\n", \
               __FILE__, __LINE__, msg, errno); \
        __pass++; \
    } else { \
        printf("  FAIL | %s:%d | %s | expected errno=%d got ret=%ld errno=%d (%s)\n", \
               __FILE__, __LINE__, msg, (int)(exp_errno), _r, errno, strerror(errno)); \
        __fail++; \
    } \
} while(0)

int main(void)
{
    printf("================================================\n");
    printf("  TEST: bug-writev-dir-ebadf\n");
    printf("================================================\n");

    char dummy[] = "test";
    struct iovec iov[1];
    iov[0].iov_base = dummy;
    iov[0].iov_len = 4;

    /* 1. writev on O_RDONLY directory => EBADF (not EISDIR) */
    int fd = open("/tmp", O_RDONLY | O_DIRECTORY);
    CHECK(fd >= 0, "open /tmp as O_RDONLY|O_DIRECTORY");
    CHECK_ERR(writev(fd, iov, 1), EBADF, "writev on directory => EBADF");
    close(fd);

    /* 2. pwrite64 on O_RDONLY directory => EBADF (not EISDIR) */
    fd = open("/tmp", O_RDONLY | O_DIRECTORY);
    CHECK(fd >= 0, "open /tmp as O_RDONLY|O_DIRECTORY (2)");
    CHECK_ERR(pwrite(fd, dummy, 4, 0), EBADF, "pwrite64 on directory => EBADF");
    close(fd);

    printf("------------------------------------------------\n");
    printf("  DONE: %d pass, %d fail\n", __pass, __fail);
    printf("================================================\n");
    if (__fail == 0) printf("ALL TESTS PASSED\n");
    return __fail > 0 ? 1 : 0;
}
