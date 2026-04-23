/*
 * bug-preadv2-offset-neg1: Reproduction test for preadv2/pwritev2 offset=-1
 *
 * Bug: StarryOS does not support offset=-1 for preadv2/pwritev2.
 * Per Linux man page, offset=-1 means "use the current file position"
 * (like readv/writev). StarryOS returns -1 with errno=0.
 *
 * Covers: preadv2 offset=-1 on file, pwritev2 offset=-1 on file,
 *         preadv2 offset=-1 on pipe, pwritev2 offset=-1 on pipe.
 */

#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/uio.h>
#include <sys/syscall.h>

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

#define CHECK_RET(call, expected, msg) do { \
    errno = 0; \
    long _r = (long)(call); \
    long _e = (long)(expected); \
    if (_r == _e) { \
        printf("  PASS | %s:%d | %s (ret=%ld)\n", \
               __FILE__, __LINE__, msg, _r); \
        __pass++; \
    } else { \
        printf("  FAIL | %s:%d | %s | expected=%ld got=%ld | errno=%d (%s)\n", \
               __FILE__, __LINE__, msg, _e, _r, errno, strerror(errno)); \
        __fail++; \
    } \
} while(0)

#define TMPFILE "/tmp/starry_bug_preadv2_neg1"

static ssize_t my_preadv2(int fd, const struct iovec *iov, int iovcnt,
                           off_t offset, int flags)
{
    return syscall(SYS_preadv2, fd, iov, iovcnt,
                   (unsigned long)offset, (unsigned long)0, flags);
}

static ssize_t my_pwritev2(int fd, const struct iovec *iov, int iovcnt,
                            off_t offset, int flags)
{
    return syscall(SYS_pwritev2, fd, iov, iovcnt,
                   (unsigned long)offset, (unsigned long)0, flags);
}

int main(void)
{
    printf("================================================\n");
    printf("  TEST: bug-preadv2-offset-neg1\n");
    printf("================================================\n");

    char buf[32];
    struct iovec iov[1];
    ssize_t n;

    /* 1. preadv2 offset=-1 on regular file: reads from current position */
    int fd = open(TMPFILE, O_RDWR | O_CREAT | O_TRUNC, 0644);
    CHECK(fd >= 0, "create temp file");
    write(fd, "ABCDEFGH", 8);
    lseek(fd, 4, SEEK_SET);

    memset(buf, 0, sizeof(buf));
    iov[0].iov_base = buf;
    iov[0].iov_len = 4;
    n = my_preadv2(fd, iov, 1, -1, 0);
    CHECK_RET(n, 4, "preadv2 offset=-1 on file reads from pos 4");
    CHECK(memcmp(buf, "EFGH", 4) == 0, "preadv2 offset=-1 data = EFGH");

    /* 2. pwritev2 offset=-1 on regular file: writes at current position */
    lseek(fd, 2, SEEK_SET);
    char wdata[] = "QQ";
    iov[0].iov_base = wdata;
    iov[0].iov_len = 2;
    n = my_pwritev2(fd, iov, 1, -1, 0);
    CHECK_RET(n, 2, "pwritev2 offset=-1 on file writes at pos 2");

    memset(buf, 0, sizeof(buf));
    pread(fd, buf, 8, 0);
    CHECK(memcmp(buf, "ABQQEFGH", 8) == 0, "pwritev2 offset=-1 data correct");
    close(fd);

    /* 3. preadv2 offset=-1 on pipe: reads like readv */
    int pipefd[2];
    CHECK(pipe(pipefd) == 0, "create pipe");
    write(pipefd[1], "PIPE", 4);

    memset(buf, 0, sizeof(buf));
    iov[0].iov_base = buf;
    iov[0].iov_len = 4;
    n = my_preadv2(pipefd[0], iov, 1, -1, 0);
    CHECK_RET(n, 4, "preadv2 offset=-1 on pipe reads OK");
    CHECK(memcmp(buf, "PIPE", 4) == 0, "preadv2 pipe data = PIPE");

    /* 4. pwritev2 offset=-1 on pipe: writes like writev */
    char pdata[] = "WRIT";
    iov[0].iov_base = pdata;
    iov[0].iov_len = 4;
    n = my_pwritev2(pipefd[1], iov, 1, -1, 0);
    CHECK_RET(n, 4, "pwritev2 offset=-1 on pipe writes OK");

    close(pipefd[0]);
    close(pipefd[1]);
    unlink(TMPFILE);

    printf("------------------------------------------------\n");
    printf("  DONE: %d pass, %d fail\n", __pass, __fail);
    printf("================================================\n");
    if (__fail == 0) printf("ALL TESTS PASSED\n");
    return __fail > 0 ? 1 : 0;
}
