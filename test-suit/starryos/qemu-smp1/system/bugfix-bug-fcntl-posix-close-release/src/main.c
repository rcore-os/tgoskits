/*
 * bug-fcntl-posix-close-release: POSIX advisory record locks must be
 * released when ANY fd referring to the same inode is closed by the
 * lock-holding process — even if the lock was acquired through a
 * different fd ("close-eats-locks", `man 2 fcntl` §"Record locking").
 *
 * Linux implements this in `fs/locks.c` via `locks_remove_posix()`
 * driven from `filp_close()`; without the equivalent hook in our
 * `close_file_like` / `sys_close_range` / execve CLOEXEC paths,
 * holding a POSIX lock through fdA and then closing an unrelated
 * fdB on the same inode would leak the lock and block subsequent
 * acquirers in the same process.
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

static int posix_lock(int fd, short type, off_t start, off_t len) {
    struct flock fl = {
        .l_type = type,
        .l_whence = SEEK_SET,
        .l_start = start,
        .l_len = len,
    };
    return fcntl(fd, F_SETLK, &fl);
}

int main(void) {
    printf("=== bug-fcntl-posix-close-release ===\n");

    const char *path = "/tmp/starry_bug_fcntl_close_release";
    int fdA = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fdA < 0) { printf("FAIL: open fdA: %s\n", strerror(errno)); return 1; }

    /* Take a write lock through fdA. */
    int r = posix_lock(fdA, F_WRLCK, 0, 100);
    CHECK(r == 0, "fdA F_WRLCK[0,100) succeeds (rc=%d errno=%d)", r, errno);

    /* Open a SECOND fd to the same file. Don't take any lock through it. */
    int fdB = open(path, O_RDWR);
    if (fdB < 0) { printf("FAIL: open fdB: %s\n", strerror(errno));
                   close(fdA); unlink(path); return 1; }

    /* Close fdB. Per POSIX/Linux, this releases ALL POSIX locks held by
     * the calling pid on this inode — including the one taken through
     * fdA. (Yes, this is the well-known POSIX foot-gun.) */
    close(fdB);

    /* fdC = fresh open. F_GETLK should report no other holder, because
     * the close(fdB) ate fdA's lock. */
    int fdC = open(path, O_RDWR);
    if (fdC < 0) { printf("FAIL: open fdC: %s\n", strerror(errno));
                   close(fdA); unlink(path); return 1; }

    struct flock g = {
        .l_type = F_WRLCK,
        .l_whence = SEEK_SET,
        .l_start = 0,
        .l_len = 100,
    };
    r = fcntl(fdC, F_GETLK, &g);
    CHECK(r == 0 && g.l_type == F_UNLCK,
          "F_GETLK reports no holder after close(fdB) ate fdA's lock "
          "(rc=%d l_type=%d)", r, (int)g.l_type);

    /* And fdC can take the same range. */
    r = posix_lock(fdC, F_WRLCK, 0, 100);
    CHECK(r == 0,
          "fdC F_WRLCK[0,100) succeeds after close-eats-locks (rc=%d errno=%d)",
          r, errno);

    (void)posix_lock(fdC, F_UNLCK, 0, 100);
    close(fdA);
    close(fdC);
    unlink(path);

    printf("\n=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) { printf("ALL TESTS PASSED\n"); return 0; }
    printf("SOME TESTS FAILED\n");
    return 1;
}
