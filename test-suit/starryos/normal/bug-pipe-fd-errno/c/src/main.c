/*
 * bug-pipe-fd-errno: Verify that syscalls which require a regular file fd
 * return the correct errno when given a pipe fd instead.
 *
 * Linux behavior:
 *   - lseek on pipe       → ESPIPE
 *   - pread on pipe       → ESPIPE
 *   - pwrite on pipe      → ESPIPE
 *   - ftruncate on pipe   → EINVAL
 *   - fsync on pipe       → EINVAL
 *   - fdatasync on pipe   → EINVAL
 *   - fadvise on pipe     → ESPIPE
 *
 * StarryOS bug: File::from_fd() returns AxError::BrokenPipe (EPIPE)
 * for all of these instead of the correct per-syscall errno.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <sys/uio.h>

static int passed = 0;
static int failed = 0;

#define TEST_ERRNO(name, call, expected_errno) do {                    \
    errno = 0;                                                         \
    long _r = (long)(call);                                            \
    if (_r == -1 && errno == (expected_errno)) {                       \
        printf("  PASS: %s (errno=%d %s)\n",                          \
               name, errno, strerror(errno));                          \
        passed++;                                                      \
    } else {                                                           \
        printf("  FAIL: %s (ret=%ld errno=%d %s, expected errno=%d %s)\n", \
               name, _r, errno, strerror(errno),                      \
               (expected_errno), strerror(expected_errno));            \
        failed++;                                                      \
    }                                                                  \
} while (0)

int main(void)
{
    printf("=== bug-pipe-fd-errno ===\n");
    printf("Test: syscalls on pipe fds return correct errno\n\n");

    int pfd[2];
    if (pipe(pfd) != 0) {
        printf("FAIL: pipe() failed: %s\n", strerror(errno));
        printf("TEST FAILED\n");
        return 1;
    }
    int rfd = pfd[0]; /* read end */
    int wfd = pfd[1]; /* write end */
    char buf[16];

    /* ── ESPIPE group: seek-like operations on pipes ── */
    printf("[ESPIPE: seek operations on pipe]\n");

    TEST_ERRNO("lseek(pipe_rd, 0, SEEK_SET)",
               lseek(rfd, 0, SEEK_SET), ESPIPE);

    TEST_ERRNO("lseek(pipe_wr, 0, SEEK_CUR)",
               lseek(wfd, 0, SEEK_CUR), ESPIPE);

    TEST_ERRNO("lseek(pipe_rd, 0, SEEK_END)",
               lseek(rfd, 0, SEEK_END), ESPIPE);

    TEST_ERRNO("pread(pipe_rd, buf, 1, 0)",
               pread(rfd, buf, 1, 0), ESPIPE);

    TEST_ERRNO("pwrite(pipe_wr, \"x\", 1, 0)",
               pwrite(wfd, "x", 1, 0), ESPIPE);

    /* preadv/pwritev with offset also require seekable fd */
    struct iovec iov = { .iov_base = buf, .iov_len = 1 };
    TEST_ERRNO("preadv(pipe_rd, &iov, 1, 0)",
               preadv(rfd, &iov, 1, 0), ESPIPE);

    TEST_ERRNO("pwritev(pipe_wr, &iov, 1, 0)",
               pwritev(wfd, &iov, 1, 0), ESPIPE);

    /* fadvise on pipe → ESPIPE
     * Note: posix_fadvise returns the error code directly, not via errno */
    {
        int rc = posix_fadvise(rfd, 0, 0, POSIX_FADV_NORMAL);
        if (rc == ESPIPE) {
            printf("  PASS: posix_fadvise(pipe) returned ESPIPE (%d)\n", rc);
            passed++;
        } else {
            printf("  FAIL: posix_fadvise(pipe) returned %d (%s), expected ESPIPE (%d)\n",
                   rc, strerror(rc), ESPIPE);
            failed++;
        }
    }

    /* ── EINVAL group: operations that need a regular file ── */
    printf("\n[EINVAL: file operations on pipe]\n");

    TEST_ERRNO("ftruncate(pipe_wr, 0)",
               ftruncate(wfd, 0), EINVAL);

    TEST_ERRNO("fsync(pipe_wr)",
               fsync(wfd), EINVAL);

    TEST_ERRNO("fdatasync(pipe_wr)",
               fdatasync(wfd), EINVAL);

    close(rfd);
    close(wfd);

    printf("\n=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("ALL TESTS PASSED\n");
        return 0;
    } else {
        printf("SOME TESTS FAILED\n");
        return 1;
    }
}
