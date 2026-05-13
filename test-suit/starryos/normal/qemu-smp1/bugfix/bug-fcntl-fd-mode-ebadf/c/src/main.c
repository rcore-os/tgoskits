/*
 * bug-fcntl-fd-mode-ebadf: installing a POSIX/OFD record lock requires
 * the fd to be open for the matching access mode — F_RDLCK needs a
 * readable fd, F_WRLCK needs a writable fd. Mismatch returns EBADF on
 * Linux (`fs/locks.c::do_lock_file_wait()` chained from `fcntl_setlk`).
 *
 * Buggy implementations let an O_RDONLY fd take a write lock or an
 * O_WRONLY fd take a read lock, which then misleads peers about who
 * can actually exercise the locked region.
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

static int posix_setlk(int cmd, int fd, short type) {
    struct flock fl = {
        .l_type = type,
        .l_whence = SEEK_SET,
        .l_start = 0,
        .l_len = 100,
    };
    return fcntl(fd, cmd, &fl);
}

int main(void) {
    printf("=== bug-fcntl-fd-mode-ebadf ===\n");

    const char *path = "/tmp/starry_bug_fcntl_fd_mode";
    int seed = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (seed < 0) { printf("FAIL: seed open: %s\n", strerror(errno)); return 1; }
    close(seed);

    int rofd = open(path, O_RDONLY);
    int wofd = open(path, O_WRONLY);
    int rwfd = open(path, O_RDWR);
    if (rofd < 0 || wofd < 0 || rwfd < 0) {
        printf("FAIL: open variants: ro=%d wo=%d rw=%d\n", rofd, wofd, rwfd);
        return 1;
    }

    /* (1) O_RDONLY fd asking for F_WRLCK -> EBADF (POSIX setlk). */
    errno = 0;
    int r = posix_setlk(F_SETLK, rofd, F_WRLCK);
    CHECK(r == -1 && errno == EBADF,
          "O_RDONLY + F_SETLK F_WRLCK -> EBADF (rc=%d errno=%d)", r, errno);

    /* (2) O_WRONLY fd asking for F_RDLCK -> EBADF (POSIX setlk). */
    errno = 0;
    r = posix_setlk(F_SETLK, wofd, F_RDLCK);
    CHECK(r == -1 && errno == EBADF,
          "O_WRONLY + F_SETLK F_RDLCK -> EBADF (rc=%d errno=%d)", r, errno);

    /* (3) Same checks must apply to the OFD command family. */
    errno = 0;
    r = posix_setlk(F_OFD_SETLK, rofd, F_WRLCK);
    CHECK(r == -1 && errno == EBADF,
          "O_RDONLY + F_OFD_SETLK F_WRLCK -> EBADF (rc=%d errno=%d)", r, errno);

    errno = 0;
    r = posix_setlk(F_OFD_SETLK, wofd, F_RDLCK);
    CHECK(r == -1 && errno == EBADF,
          "O_WRONLY + F_OFD_SETLK F_RDLCK -> EBADF (rc=%d errno=%d)", r, errno);

    /* (4) Sanity: matching mode succeeds. */
    errno = 0;
    r = posix_setlk(F_SETLK, rofd, F_RDLCK);
    CHECK(r == 0,
          "O_RDONLY + F_SETLK F_RDLCK succeeds (rc=%d errno=%d)", r, errno);
    (void)posix_setlk(F_SETLK, rofd, F_UNLCK);

    errno = 0;
    r = posix_setlk(F_SETLK, wofd, F_WRLCK);
    CHECK(r == 0,
          "O_WRONLY + F_SETLK F_WRLCK succeeds (rc=%d errno=%d)", r, errno);
    (void)posix_setlk(F_SETLK, wofd, F_UNLCK);

    /* (5) F_UNLCK on a wrong-mode fd is allowed (release is unrestricted). */
    errno = 0;
    r = posix_setlk(F_SETLK, rofd, F_UNLCK);
    CHECK(r == 0,
          "O_RDONLY + F_SETLK F_UNLCK is permitted (rc=%d errno=%d)", r, errno);

    /* (6) Sanity for OFD with rwfd. */
    errno = 0;
    r = posix_setlk(F_OFD_SETLK, rwfd, F_WRLCK);
    CHECK(r == 0,
          "O_RDWR + F_OFD_SETLK F_WRLCK succeeds (rc=%d errno=%d)", r, errno);
    (void)posix_setlk(F_OFD_SETLK, rwfd, F_UNLCK);

    close(rofd);
    close(wofd);
    close(rwfd);
    unlink(path);

    printf("\n=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) { printf("ALL TESTS PASSED\n"); return 0; }
    printf("SOME TESTS FAILED\n");
    return 1;
}
