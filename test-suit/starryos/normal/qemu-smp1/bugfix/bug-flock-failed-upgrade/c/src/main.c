/*
 * bug-flock-failed-upgrade: flock(2) upgrade from LOCK_SH to LOCK_EX
 * must NOT drop the original LOCK_SH if the upgrade fails.
 *
 * Buggy implementations remove the holder's existing entry before
 * checking conflicts; if the EX upgrade then loses to a peer's LOCK_SH
 * and returns EWOULDBLOCK, the original LOCK_SH is silently gone and
 * subsequent acquirers see the file as unlocked.
 *
 * Layout:
 *   fd1 LOCK_SH            -> ok
 *   fd2 LOCK_SH            -> ok (compatible with fd1)
 *   fd1 LOCK_EX | LOCK_NB  -> EWOULDBLOCK (fd2 is in the way)
 *   close(fd2)             -> only fd1's LOCK_SH should remain
 *   fd3 = open(); fd3 LOCK_EX | LOCK_NB -> EWOULDBLOCK
 *      ^^^ this is the regression: passes only if fd1 still holds SH
 *   fd1 LOCK_UN; fd3 LOCK_EX | LOCK_NB -> ok
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

    /* The failed upgrade. Must not eat fd1's LOCK_SH. */
    errno = 0;
    r = flock(fd1, LOCK_EX | LOCK_NB);
    CHECK(r == -1 && errno == EWOULDBLOCK,
          "fd1 LOCK_EX|LOCK_NB returns EWOULDBLOCK (rc=%d errno=%d)", r, errno);

    /* Drop fd2's LOCK_SH so the only remaining holder *should* be fd1. */
    (void)flock(fd2, LOCK_UN);
    close(fd2);

    /* Fresh OFD via fd3. If fd1's LOCK_SH was preserved, fd3's LOCK_EX
     * still conflicts. If the bug was present, fd1's LOCK_SH is gone and
     * fd3 LOCK_EX would succeed — which would FAIL this assertion. */
    int fd3 = open(path, O_RDWR);
    if (fd3 < 0) { printf("FAIL: open fd3: %s\n", strerror(errno));
                   close(fd1); unlink(path); return 1; }

    errno = 0;
    r = flock(fd3, LOCK_EX | LOCK_NB);
    CHECK(r == -1 && errno == EWOULDBLOCK,
          "fd3 LOCK_EX|LOCK_NB still blocked by fd1's preserved LOCK_SH "
          "(rc=%d errno=%d)", r, errno);

    /* Now drop fd1's LOCK_SH; fd3's LOCK_EX must finally succeed. */
    (void)flock(fd1, LOCK_UN);
    r = flock(fd3, LOCK_EX | LOCK_NB);
    CHECK(r == 0,
          "after fd1 LOCK_UN, fd3 LOCK_EX|LOCK_NB succeeds (rc=%d errno=%d)",
          r, errno);

    (void)flock(fd3, LOCK_UN);
    close(fd1);
    close(fd3);
    unlink(path);

    printf("\n=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) { printf("ALL TESTS PASSED\n"); return 0; }
    printf("SOME TESTS FAILED\n");
    return 1;
}
