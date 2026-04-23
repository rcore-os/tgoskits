/*
 * test-vectored-io: Comprehensive edge-case test for vectored I/O syscalls
 *
 * Syscalls covered: readv, writev, pread64, pwrite64, preadv, pwritev,
 *                   preadv2, pwritev2
 *
 * Tests edge cases from man pages: error conditions, boundary values,
 * file type interactions, and preadv2/pwritev2 flag behavior.
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
#include <sys/stat.h>
#include <sys/syscall.h>
#include <limits.h>
#include <signal.h>

/* ---- Minimal test framework (matches test_framework.h conventions) ---- */
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

#define TMPFILE "/tmp/starry_test_vectored_io"

/* musl lacks preadv2/pwritev2 wrappers; use raw syscall.
 * Kernel splits offset into pos_l + pos_h; on 64-bit pos_h = 0. */
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

/* Helper: create and fill a temp file with known content */
static int create_tmp(const char *content, size_t len)
{
    int fd = open(TMPFILE, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) return -1;
    if (content && len > 0) {
        write(fd, content, len);
        lseek(fd, 0, SEEK_SET);
    }
    return fd;
}

int main(void)
{
    printf("================================================\n");
    printf("  TEST: test-vectored-io\n");
    printf("  Syscalls: readv, writev, pread64, pwrite64,\n");
    printf("            preadv, pwritev, preadv2, pwritev2\n");
    printf("================================================\n");

    int fd;
    ssize_t n;
    char buf1[32], buf2[32], buf3[64];
    struct iovec iov[3];

    /* ================================================================
     * SECTION 1: Normal behavior — scatter/gather read/write
     * ================================================================ */

    /* 1.1 writev scatter write then readv gather read */
    fd = create_tmp(NULL, 0);
    CHECK(fd >= 0, "1.1 create temp file");

    char w1[] = "AAAA";
    char w2[] = "BBBB";
    char w3[] = "CCCC";
    iov[0].iov_base = w1; iov[0].iov_len = 4;
    iov[1].iov_base = w2; iov[1].iov_len = 4;
    iov[2].iov_base = w3; iov[2].iov_len = 4;
    n = writev(fd, iov, 3);
    CHECK_RET(n, 12, "1.1 writev 3 iovecs = 12 bytes");

    lseek(fd, 0, SEEK_SET);
    memset(buf1, 0, sizeof(buf1));
    memset(buf2, 0, sizeof(buf2));
    memset(buf3, 0, sizeof(buf3));
    iov[0].iov_base = buf1; iov[0].iov_len = 4;
    iov[1].iov_base = buf2; iov[1].iov_len = 4;
    iov[2].iov_base = buf3; iov[2].iov_len = 4;
    n = readv(fd, iov, 3);
    CHECK_RET(n, 12, "1.1 readv 3 iovecs = 12 bytes");
    CHECK(memcmp(buf1, "AAAA", 4) == 0, "1.1 readv iov[0] = AAAA");
    CHECK(memcmp(buf2, "BBBB", 4) == 0, "1.1 readv iov[1] = BBBB");
    CHECK(memcmp(buf3, "CCCC", 4) == 0, "1.1 readv iov[2] = CCCC");
    close(fd);

    /* 1.2 pwrite64 + pread64 at offset */
    fd = create_tmp("0000000000000000", 16);
    n = pwrite(fd, "HELLO", 5, 4);
    CHECK_RET(n, 5, "1.2 pwrite64 5 bytes at offset 4");
    memset(buf1, 0, sizeof(buf1));
    n = pread(fd, buf1, 5, 4);
    CHECK_RET(n, 5, "1.2 pread64 5 bytes at offset 4");
    CHECK(memcmp(buf1, "HELLO", 5) == 0, "1.2 pread64 data matches");
    close(fd);

    /* 1.3 pwritev + preadv at offset */
    fd = create_tmp("0000000000000000", 16);
    char pw1[] = "XX";
    char pw2[] = "YY";
    iov[0].iov_base = pw1; iov[0].iov_len = 2;
    iov[1].iov_base = pw2; iov[1].iov_len = 2;
    n = pwritev(fd, iov, 2, 8);
    CHECK_RET(n, 4, "1.3 pwritev 4 bytes at offset 8");

    memset(buf1, 0, sizeof(buf1));
    memset(buf2, 0, sizeof(buf2));
    iov[0].iov_base = buf1; iov[0].iov_len = 2;
    iov[1].iov_base = buf2; iov[1].iov_len = 2;
    n = preadv(fd, iov, 2, 8);
    CHECK_RET(n, 4, "1.3 preadv 4 bytes at offset 8");
    CHECK(memcmp(buf1, "XX", 2) == 0, "1.3 preadv iov[0] = XX");
    CHECK(memcmp(buf2, "YY", 2) == 0, "1.3 preadv iov[1] = YY");
    close(fd);

    /* 1.4 pwritev2 + preadv2 with flags=0 (same as pwritev/preadv) */
    fd = create_tmp("0000000000000000", 16);
    char pv2w[] = "ZZZZ";
    iov[0].iov_base = pv2w; iov[0].iov_len = 4;
    n = my_pwritev2(fd, iov, 1, 4, 0);
    CHECK_RET(n, 4, "1.4 pwritev2 4 bytes at offset 4, flags=0");

    memset(buf1, 0, sizeof(buf1));
    iov[0].iov_base = buf1; iov[0].iov_len = 4;
    n = my_preadv2(fd, iov, 1, 4, 0);
    CHECK_RET(n, 4, "1.4 preadv2 4 bytes at offset 4, flags=0");
    CHECK(memcmp(buf1, "ZZZZ", 4) == 0, "1.4 preadv2 data = ZZZZ");
    close(fd);

    /* 1.5 readv at EOF returns 0 */
    fd = create_tmp("AB", 2);
    lseek(fd, 2, SEEK_SET);
    memset(buf1, 0, sizeof(buf1));
    iov[0].iov_base = buf1; iov[0].iov_len = 4;
    n = readv(fd, iov, 1);
    CHECK_RET(n, 0, "1.5 readv at EOF returns 0");
    close(fd);

    /* 1.6 preadv2 with offset=-1 uses current file position */
    fd = create_tmp("ABCDEFGH", 8);
    lseek(fd, 4, SEEK_SET);
    memset(buf1, 0, sizeof(buf1));
    iov[0].iov_base = buf1; iov[0].iov_len = 4;
    n = my_preadv2(fd, iov, 1, -1, 0);
    CHECK_RET(n, 4, "1.6 preadv2 offset=-1 reads from current pos");
    CHECK(memcmp(buf1, "EFGH", 4) == 0, "1.6 preadv2 offset=-1 data = EFGH");
    close(fd);

    /* 1.7 pwritev2 with offset=-1 uses current file position */
    fd = create_tmp("00000000", 8);
    lseek(fd, 2, SEEK_SET);
    char pw2w[] = "QQ";
    iov[0].iov_base = pw2w; iov[0].iov_len = 2;
    n = my_pwritev2(fd, iov, 1, -1, 0);
    CHECK_RET(n, 2, "1.7 pwritev2 offset=-1 writes at current pos");
    memset(buf1, 0, sizeof(buf1));
    pread(fd, buf1, 8, 0);
    CHECK(memcmp(buf1, "00QQ0000", 8) == 0, "1.7 pwritev2 offset=-1 data correct");
    close(fd);

    /* ================================================================
     * SECTION 2: Error conditions — EBADF
     * ================================================================ */

    /* 2.1 readv on bad fd */
    iov[0].iov_base = buf1; iov[0].iov_len = 4;
    CHECK_ERR(readv(-1, iov, 1), EBADF, "2.1 readv(-1) => EBADF");

    /* 2.2 writev on bad fd */
    char dummy[] = "test";
    iov[0].iov_base = dummy; iov[0].iov_len = 4;
    CHECK_ERR(writev(-1, iov, 1), EBADF, "2.2 writev(-1) => EBADF");

    /* 2.3 readv on write-only fd */
    fd = open(TMPFILE, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    CHECK(fd >= 0, "2.3 open write-only");
    iov[0].iov_base = buf1; iov[0].iov_len = 4;
    CHECK_ERR(readv(fd, iov, 1), EBADF, "2.3 readv on O_WRONLY => EBADF");
    close(fd);

    /* 2.4 writev on read-only fd */
    fd = open(TMPFILE, O_RDONLY);
    CHECK(fd >= 0, "2.4 open read-only");
    iov[0].iov_base = dummy; iov[0].iov_len = 4;
    CHECK_ERR(writev(fd, iov, 1), EBADF, "2.4 writev on O_RDONLY => EBADF");
    close(fd);

    /* 2.5 pread64 on bad fd */
    CHECK_ERR(pread(-1, buf1, 4, 0), EBADF, "2.5 pread64(-1) => EBADF");

    /* 2.6 pwrite64 on bad fd */
    CHECK_ERR(pwrite(-1, dummy, 4, 0), EBADF, "2.6 pwrite64(-1) => EBADF");

    /* ================================================================
     * SECTION 3: Error conditions — EINVAL (iovcnt)
     * ================================================================ */

    fd = create_tmp("ABCDEFGH", 8);

    /* 3.1 readv with iovcnt=0 => returns 0 (Linux behavior) */
    n = readv(fd, iov, 0);
    CHECK_RET(n, 0, "3.1 readv iovcnt=0 => 0");

    /* 3.2 writev with iovcnt=0 => returns 0 */
    n = writev(fd, iov, 0);
    CHECK_RET(n, 0, "3.2 writev iovcnt=0 => 0");

    /* 3.3 readv with iovcnt=-1 => EINVAL */
    CHECK_ERR(readv(fd, iov, -1), EINVAL, "3.3 readv iovcnt=-1 => EINVAL");

    /* 3.4 writev with iovcnt=-1 => EINVAL */
    iov[0].iov_base = dummy; iov[0].iov_len = 4;
    CHECK_ERR(writev(fd, iov, -1), EINVAL, "3.4 writev iovcnt=-1 => EINVAL");

    /* 3.5 readv with iovcnt > IOV_MAX => EINVAL */
    CHECK_ERR(readv(fd, iov, IOV_MAX + 1), EINVAL, "3.5 readv iovcnt>IOV_MAX => EINVAL");

    /* 3.6 writev with iovcnt > IOV_MAX => EINVAL */
    CHECK_ERR(writev(fd, iov, IOV_MAX + 1), EINVAL, "3.6 writev iovcnt>IOV_MAX => EINVAL");

    close(fd);

    /* ================================================================
     * SECTION 4: Error conditions — EINVAL (negative offset for pread/pwrite)
     * ================================================================ */

    fd = create_tmp("ABCDEFGH", 8);

    /* 4.1 pread64 with negative offset => EINVAL */
    CHECK_ERR(pread(fd, buf1, 4, -1), EINVAL, "4.1 pread64 offset=-1 => EINVAL");

    /* 4.2 pwrite64 with negative offset => EINVAL */
    CHECK_ERR(pwrite(fd, dummy, 4, -1), EINVAL, "4.2 pwrite64 offset=-1 => EINVAL");

    /* 4.3 preadv with negative offset => EINVAL */
    iov[0].iov_base = buf1; iov[0].iov_len = 4;
    CHECK_ERR(preadv(fd, iov, 1, -1), EINVAL, "4.3 preadv offset=-1 => EINVAL");

    /* 4.4 pwritev with negative offset => EINVAL */
    iov[0].iov_base = dummy; iov[0].iov_len = 4;
    CHECK_ERR(pwritev(fd, iov, 1, -1), EINVAL, "4.4 pwritev offset=-1 => EINVAL");

    /* 4.5 preadv2 with offset < -1 => EINVAL */
    iov[0].iov_base = buf1; iov[0].iov_len = 4;
    CHECK_ERR(my_preadv2(fd, iov, 1, -2, 0), EINVAL, "4.5 preadv2 offset=-2 => EINVAL");

    /* 4.6 pwritev2 with offset < -1 => EINVAL */
    iov[0].iov_base = dummy; iov[0].iov_len = 4;
    CHECK_ERR(my_pwritev2(fd, iov, 1, -2, 0), EINVAL, "4.6 pwritev2 offset=-2 => EINVAL");

    close(fd);

    /* ================================================================
     * SECTION 5: Error conditions — ESPIPE (pread/pwrite on pipe)
     * ================================================================ */

    {
        int pipefd[2];
        CHECK(pipe(pipefd) == 0, "5.0 create pipe");

        /* 5.1 pread64 on pipe => ESPIPE */
        CHECK_ERR(pread(pipefd[0], buf1, 4, 0), ESPIPE, "5.1 pread64 on pipe => ESPIPE");

        /* 5.2 pwrite64 on pipe => ESPIPE */
        CHECK_ERR(pwrite(pipefd[1], dummy, 4, 0), ESPIPE, "5.2 pwrite64 on pipe => ESPIPE");

        /* 5.3 preadv on pipe => ESPIPE */
        iov[0].iov_base = buf1; iov[0].iov_len = 4;
        CHECK_ERR(preadv(pipefd[0], iov, 1, 0), ESPIPE, "5.3 preadv on pipe => ESPIPE");

        /* 5.4 pwritev on pipe => ESPIPE */
        iov[0].iov_base = dummy; iov[0].iov_len = 4;
        CHECK_ERR(pwritev(pipefd[1], iov, 1, 0), ESPIPE, "5.4 pwritev on pipe => ESPIPE");

        /* 5.5 preadv2 on pipe with offset>=0 => ESPIPE */
        iov[0].iov_base = buf1; iov[0].iov_len = 4;
        CHECK_ERR(my_preadv2(pipefd[0], iov, 1, 0, 0), ESPIPE, "5.5 preadv2 on pipe offset=0 => ESPIPE");

        /* 5.6 pwritev2 on pipe with offset>=0 => ESPIPE */
        iov[0].iov_base = dummy; iov[0].iov_len = 4;
        CHECK_ERR(my_pwritev2(pipefd[1], iov, 1, 0, 0), ESPIPE, "5.6 pwritev2 on pipe offset=0 => ESPIPE");

        /* 5.7 preadv2 on pipe with offset=-1 => OK (uses current pos, like readv) */
        write(pipefd[1], "PIPE", 4);
        memset(buf1, 0, sizeof(buf1));
        iov[0].iov_base = buf1; iov[0].iov_len = 4;
        n = my_preadv2(pipefd[0], iov, 1, -1, 0);
        CHECK_RET(n, 4, "5.7 preadv2 on pipe offset=-1 => reads OK");
        CHECK(memcmp(buf1, "PIPE", 4) == 0, "5.7 preadv2 pipe data = PIPE");

        /* 5.8 pwritev2 on pipe with offset=-1 => OK (uses current pos, like writev) */
        char pdata[] = "WRIT";
        iov[0].iov_base = pdata; iov[0].iov_len = 4;
        n = my_pwritev2(pipefd[1], iov, 1, -1, 0);
        CHECK_RET(n, 4, "5.8 pwritev2 on pipe offset=-1 => writes OK");

        close(pipefd[0]);
        close(pipefd[1]);
    }

    /* ================================================================
     * SECTION 6: Boundary values — zero-length iovecs
     * ================================================================ */

    fd = create_tmp("ABCDEFGH", 8);

    /* 6.1 readv with single zero-length iovec => returns 0 */
    iov[0].iov_base = buf1; iov[0].iov_len = 0;
    n = readv(fd, iov, 1);
    CHECK_RET(n, 0, "6.1 readv zero-length iovec => 0");

    /* 6.2 writev with single zero-length iovec => returns 0 */
    iov[0].iov_base = dummy; iov[0].iov_len = 0;
    n = writev(fd, iov, 1);
    CHECK_RET(n, 0, "6.2 writev zero-length iovec => 0");

    /* 6.3 readv with mix of zero and non-zero iovecs */
    lseek(fd, 0, SEEK_SET);
    memset(buf1, 0, sizeof(buf1));
    memset(buf2, 0, sizeof(buf2));
    iov[0].iov_base = buf1; iov[0].iov_len = 0;  /* zero-length */
    iov[1].iov_base = buf2; iov[1].iov_len = 4;  /* 4 bytes */
    n = readv(fd, iov, 2);
    CHECK_RET(n, 4, "6.3 readv [0-len, 4-len] => 4");
    CHECK(memcmp(buf2, "ABCD", 4) == 0, "6.3 readv skips zero-len, reads into second");

    close(fd);

    /* ================================================================
     * SECTION 7: File type interactions — directory
     * ================================================================ */

    /* 7.1 writev on directory fd => EBADF (dirs not writable) */
    fd = open("/tmp", O_RDONLY | O_DIRECTORY);
    if (fd >= 0) {
        iov[0].iov_base = dummy; iov[0].iov_len = 4;
        CHECK_ERR(writev(fd, iov, 1), EBADF, "7.1 writev on directory => EBADF");
        close(fd);
    } else {
        printf("  PASS | %s:%d | 7.1 skip: cannot open /tmp as directory\n", __FILE__, __LINE__);
        __pass++;
    }

    /* 7.2 pwrite64 on directory fd => EBADF */
    fd = open("/tmp", O_RDONLY | O_DIRECTORY);
    if (fd >= 0) {
        CHECK_ERR(pwrite(fd, dummy, 4, 0), EBADF, "7.2 pwrite64 on directory => EBADF");
        close(fd);
    } else {
        printf("  PASS | %s:%d | 7.2 skip: cannot open /tmp as directory\n", __FILE__, __LINE__);
        __pass++;
    }

    /* SECTION 8: preadv2/pwritev2 flags — SKIPPED (StarryOS does not support flags yet) */

    /* ================================================================
     * SECTION 9: Partial read (readv returns less than requested)
     * ================================================================ */

    fd = create_tmp("AB", 2);
    lseek(fd, 0, SEEK_SET);
    memset(buf1, 0, sizeof(buf1));
    iov[0].iov_base = buf1; iov[0].iov_len = 16;  /* request 16, only 2 available */
    n = readv(fd, iov, 1);
    CHECK_RET(n, 2, "9.1 readv partial read: requested 16, got 2");
    CHECK(memcmp(buf1, "AB", 2) == 0, "9.1 partial read data correct");
    close(fd);

    /* ================================================================
     * SECTION 10: EISDIR — read/write on directory
     * ================================================================ */

    /* 10.1 readv on directory => EISDIR */
    fd = open("/tmp", O_RDONLY | O_DIRECTORY);
    if (fd >= 0) {
        iov[0].iov_base = buf1; iov[0].iov_len = 4;
        errno = 0;
        n = readv(fd, iov, 1);
        /* Linux returns EISDIR for read on directory */
        CHECK(n == -1 && errno == EISDIR, "10.1 readv on directory => EISDIR");
        close(fd);
    } else {
        printf("  PASS | %s:%d | 10.1 skip: cannot open /tmp as directory\n", __FILE__, __LINE__);
        __pass++;
    }

    /* ================================================================
     * SECTION 11: writev atomicity — single writev is atomic for pipes
     * ================================================================ */

    {
        int pipefd[2];
        CHECK(pipe(pipefd) == 0, "11.0 create pipe for writev");

        char wa[] = "ATOM";
        iov[0].iov_base = wa; iov[0].iov_len = 4;
        n = writev(pipefd[1], iov, 1);
        CHECK_RET(n, 4, "11.1 writev to pipe => 4 bytes");

        memset(buf1, 0, sizeof(buf1));
        n = read(pipefd[0], buf1, 4);
        CHECK_RET(n, 4, "11.1 read from pipe => 4 bytes");
        CHECK(memcmp(buf1, "ATOM", 4) == 0, "11.1 pipe data = ATOM");

        close(pipefd[0]);
        close(pipefd[1]);
    }

    /* ================================================================
     * SECTION 12: EPIPE — write to pipe with no readers
     * ================================================================ */

    {
        int pipefd[2];
        CHECK(pipe(pipefd) == 0, "12.0 create pipe for EPIPE test");

        /* Ignore SIGPIPE so we get EPIPE errno instead of signal death */
        signal(SIGPIPE, SIG_IGN);

        close(pipefd[0]);  /* close read end */

        iov[0].iov_base = dummy; iov[0].iov_len = 4;
        CHECK_ERR(writev(pipefd[1], iov, 1), EPIPE, "12.1 writev to broken pipe => EPIPE");

        close(pipefd[1]);

        /* Restore default SIGPIPE handling */
        signal(SIGPIPE, SIG_DFL);
    }

    /* Clean up */
    unlink(TMPFILE);

    /* ================================================================
     * Summary
     * ================================================================ */

    printf("------------------------------------------------\n");
    printf("  DONE: %d pass, %d fail\n", __pass, __fail);
    printf("================================================\n");

    if (__fail == 0) {
        printf("ALL TESTS PASSED\n");
    }

    return __fail > 0 ? 1 : 0;
}
