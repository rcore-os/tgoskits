/*
 * bug-preadv2-invalid-flags: preadv2/pwritev2 accept invalid RWF_* flags
 *
 * Bug: StarryOS silently accepts invalid flags (e.g. 0x80) for preadv2
 * and pwritev2 instead of returning EOPNOTSUPP.
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

#define TMPFILE "/tmp/starry_bug_invalid_flags"

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
    printf("  TEST: bug-preadv2-invalid-flags\n");
    printf("================================================\n");

    char buf[32];
    struct iovec iov[1];
    ssize_t n;
    int bad_flags = 0x80; /* not a valid RWF_* flag */

    int fd = open(TMPFILE, O_RDWR | O_CREAT | O_TRUNC, 0644);
    CHECK(fd >= 0, "create temp file");
    write(fd, "ABCD", 4);

    /* preadv2 with invalid flags => EOPNOTSUPP */
    iov[0].iov_base = buf;
    iov[0].iov_len = 4;
    errno = 0;
    n = my_preadv2(fd, iov, 1, 0, bad_flags);
    CHECK(n == -1 && (errno == EOPNOTSUPP),
          "preadv2 invalid flags => EOPNOTSUPP");

    /* pwritev2 with invalid flags => EOPNOTSUPP */
    char wdata[] = "XXXX";
    iov[0].iov_base = wdata;
    iov[0].iov_len = 4;
    errno = 0;
    n = my_pwritev2(fd, iov, 1, 0, bad_flags);
    CHECK(n == -1 && (errno == EOPNOTSUPP),
          "pwritev2 invalid flags => EOPNOTSUPP");

    close(fd);
    unlink(TMPFILE);

    printf("------------------------------------------------\n");
    printf("  DONE: %d pass, %d fail\n", __pass, __fail);
    printf("================================================\n");
    if (__fail == 0) printf("ALL TESTS PASSED\n");
    return __fail > 0 ? 1 : 0;
}
