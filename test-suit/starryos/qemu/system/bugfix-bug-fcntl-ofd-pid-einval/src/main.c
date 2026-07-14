/*
 * bug-fcntl-ofd-pid-einval: F_OFD_SETLK / F_OFD_SETLKW / F_OFD_GETLK
 * require `flock.l_pid == 0`. POSIX.1-2024 and the Linux man page
 * `fcntl(2)` "Open file description locks (non-POSIX)" both spell this
 * out, and `fs/locks.c::fcntl_setlk()` returns -EINVAL otherwise.
 *
 * A buggy implementation that silently accepts non-zero l_pid for OFD
 * commands lets userspace impersonate other processes' lock owners
 * via F_GETLK reporting and breaks ABI compatibility.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static int passed = 0;
static int failed = 0;

#define CHECK(cond, fmt, ...) do {                                  \
    if (cond) { printf("  PASS: " fmt "\n", ##__VA_ARGS__); passed++; } \
    else      { printf("  FAIL: " fmt "\n", ##__VA_ARGS__); failed++; } \
} while (0)

int main(void) {
    printf("=== bug-fcntl-ofd-pid-einval ===\n");

    const char *path = "/tmp/starry_bug_fcntl_ofd_pid";
    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) { printf("FAIL: open: %s\n", strerror(errno)); return 1; }

    /* Each OFD command must reject non-zero l_pid with EINVAL. */
    struct flock fl_set = {
        .l_type = F_WRLCK,
        .l_whence = SEEK_SET,
        .l_start = 0,
        .l_len = 100,
        .l_pid = 1,
    };
    errno = 0;
    int r = fcntl(fd, F_OFD_SETLK, &fl_set);
    CHECK(r == -1 && errno == EINVAL,
          "F_OFD_SETLK with l_pid=1 returns EINVAL (rc=%d errno=%d)", r, errno);

    errno = 0;
    r = fcntl(fd, F_OFD_SETLKW, &fl_set);
    CHECK(r == -1 && errno == EINVAL,
          "F_OFD_SETLKW with l_pid=1 returns EINVAL (rc=%d errno=%d)", r, errno);

    struct flock fl_get = {
        .l_type = F_WRLCK,
        .l_whence = SEEK_SET,
        .l_start = 0,
        .l_len = 100,
        .l_pid = 1,
    };
    errno = 0;
    r = fcntl(fd, F_OFD_GETLK, &fl_get);
    CHECK(r == -1 && errno == EINVAL,
          "F_OFD_GETLK with l_pid=1 returns EINVAL (rc=%d errno=%d)", r, errno);

    /* Sanity: same calls with l_pid=0 succeed. */
    fl_set.l_pid = 0;
    errno = 0;
    r = fcntl(fd, F_OFD_SETLK, &fl_set);
    CHECK(r == 0,
          "F_OFD_SETLK with l_pid=0 succeeds (rc=%d errno=%d)", r, errno);

    fl_get.l_pid = 0;
    errno = 0;
    r = fcntl(fd, F_OFD_GETLK, &fl_get);
    CHECK(r == 0,
          "F_OFD_GETLK with l_pid=0 succeeds (rc=%d errno=%d)", r, errno);

    /* Cleanup */
    fl_set.l_type = F_UNLCK;
    (void)fcntl(fd, F_OFD_SETLK, &fl_set);
    close(fd);
    unlink(path);

    printf("\n=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) { printf("ALL TESTS PASSED\n"); return 0; }
    printf("SOME TESTS FAILED\n");
    return 1;
}
