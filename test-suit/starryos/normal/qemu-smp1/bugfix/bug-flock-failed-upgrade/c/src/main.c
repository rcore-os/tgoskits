/*
 * bug-flock-failed-upgrade: flock(2) lock conversion is non-atomic on
 * Linux — the calling OFD's existing entry is removed BEFORE the
 * conflict check, so a failed `LOCK_NB` upgrade leaves the file with
 * no flock held (see `flock_lock_inode()` in fs/locks.c, and the
 * "Converting a lock" paragraph in `man 2 flock` NOTES).
 *
 * Buggy implementations treat the upgrade as atomic and silently
 * preserve the old LOCK_SH on conflict, masking races that real Linux
 * would expose.
 *
 * Layout:
 *   fd1 LOCK_SH                       -> ok
 *   fd2 LOCK_SH                       -> ok (compatible with fd1)
 *   fd1 LOCK_EX | LOCK_NB             -> EWOULDBLOCK
 *      ^^^ fd1's LOCK_SH is gone after this, even though the upgrade
 *      failed. Linux non-atomic conversion semantics.
 *   close(fd2)                        -> only fd2's LOCK_SH was left
 *   fd3 = open(); fd3 LOCK_EX | LOCK_NB -> ok
 *      ^^^ this is the regression: would FAIL on a buggy implementation
 *      that preserved fd1's LOCK_SH on the failed upgrade.
 *   fd1 LOCK_EX | LOCK_NB             -> EWOULDBLOCK (fd3 holds EX)
 *   fd3 LOCK_UN; fd1 LOCK_EX | LOCK_NB -> ok
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/file.h>
#include <unistd.h>

static int passed = 0;
static int failed = 0;

#define CHECK(cond, fmt, ...) do {                                  \
    if (cond) { printf("  PASS: " fmt "\n", ##__VA_ARGS__); passed++; } \
    else      { printf("  FAIL: " fmt "\n", ##__VA_ARGS__); failed++; } \
} while (0)

int main(void) {
    printf("=== bug-flock-failed-upgrade ===\n");

    const char *path = "/tmp/starry_bug_flock_upgrade";
    int fd1 = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd1 < 0) { printf("FAIL: open fd1: %s\n", strerror(errno)); return 1; }
    int fd2 = open(path, O_RDWR);
    if (fd2 < 0) { printf("FAIL: open fd2: %s\n", strerror(errno));
                   close(fd1); unlink(path); return 1; }

    int r = flock(fd1, LOCK_SH);
    CHECK(r == 0, "fd1 LOCK_SH succeeds (rc=%d errno=%d)", r, errno);

    r = flock(fd2, LOCK_SH);
    CHECK(r == 0, "fd2 LOCK_SH coexists with fd1 (rc=%d errno=%d)", r, errno);

    /* The failed upgrade. Linux removes fd1's old LOCK_SH first, then
     * detects the conflict against fd2 and returns EWOULDBLOCK without
     * restoring. */
    errno = 0;
    r = flock(fd1, LOCK_EX | LOCK_NB);
    CHECK(r == -1 && errno == EWOULDBLOCK,
          "fd1 LOCK_EX|LOCK_NB returns EWOULDBLOCK (rc=%d errno=%d)", r, errno);

    /* Drop fd2's LOCK_SH. After this, no flock is held on the file
     * (fd1's LOCK_SH was eaten by the failed upgrade). */
    (void)flock(fd2, LOCK_UN);
    close(fd2);

    /* Fresh OFD via fd3. With Linux semantics fd1 has no flock, so
     * fd3 LOCK_EX must succeed. A buggy implementation that preserved
     * fd1's LOCK_SH on the failed upgrade would have fd3 see EWOULDBLOCK. */
    int fd3 = open(path, O_RDWR);
    if (fd3 < 0) { printf("FAIL: open fd3: %s\n", strerror(errno));
                   close(fd1); unlink(path); return 1; }

    errno = 0;
    r = flock(fd3, LOCK_EX | LOCK_NB);
    CHECK(r == 0,
          "fd3 LOCK_EX|LOCK_NB succeeds — fd1's LOCK_SH was consumed by "
          "the failed upgrade (rc=%d errno=%d)", r, errno);

    /* fd1 has nothing now; trying to upgrade-from-nothing to EX is
     * blocked by fd3. */
    errno = 0;
    r = flock(fd1, LOCK_EX | LOCK_NB);
    CHECK(r == -1 && errno == EWOULDBLOCK,
          "fd1 LOCK_EX|LOCK_NB blocked by fd3's LOCK_EX (rc=%d errno=%d)",
          r, errno);

    /* Drop fd3's LOCK_EX; fd1 can now take it. */
    (void)flock(fd3, LOCK_UN);
    r = flock(fd1, LOCK_EX | LOCK_NB);
    CHECK(r == 0,
          "after fd3 LOCK_UN, fd1 LOCK_EX|LOCK_NB succeeds (rc=%d errno=%d)",
          r, errno);

    (void)flock(fd1, LOCK_UN);
    close(fd1);
    close(fd3);
    unlink(path);

    printf("\n=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) { printf("ALL TESTS PASSED\n"); return 0; }
    printf("SOME TESTS FAILED\n");
    return 1;
}
