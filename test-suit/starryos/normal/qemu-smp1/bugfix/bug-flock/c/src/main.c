/*
 * bug-flock: Verify flock(2) advisory locking honors LOCK_SH/LOCK_EX
 * exclusion semantics across two open file descriptions of the same file.
 *
 * StarryOS bug: kernel/src/syscall/fs/fd_ops.rs:299-303 stubs out
 *     pub fn sys_flock(fd, operation) -> AxResult<isize> { Ok(0) }
 * so every flock() request silently succeeds — two competing writers both
 * believe they own the exclusive lock.
 *
 * flock locks are associated with the open file description (so two open()s
 * on the same file produce independently lockable handles), and they're
 * advisory only.
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
    printf("=== bug-flock ===\n");

    const char *path = "/tmp/starry_bug_flock";
    int fd1 = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd1 < 0) {
        printf("FAIL: open fd1: %s\n", strerror(errno));
        return 1;
    }
    int fd2 = open(path, O_RDWR);
    if (fd2 < 0) {
        printf("FAIL: open fd2: %s\n", strerror(errno));
        close(fd1); unlink(path); return 1;
    }

    /* (1) LOCK_EX on fd1 succeeds */
    int r = flock(fd1, LOCK_EX);
    CHECK(r == 0, "flock(fd1, LOCK_EX) succeeds (rc=%d errno=%d)", r, errno);

    /* (2) LOCK_EX|LOCK_NB on fd2 must fail with EWOULDBLOCK */
    errno = 0;
    r = flock(fd2, LOCK_EX | LOCK_NB);
    CHECK(r == -1 && errno == EWOULDBLOCK,
          "flock(fd2, LOCK_EX|LOCK_NB) returns EWOULDBLOCK (rc=%d errno=%d)",
          r, errno);

    /* Release fd1's exclusive lock */
    r = flock(fd1, LOCK_UN);
    CHECK(r == 0, "flock(fd1, LOCK_UN) succeeds (rc=%d errno=%d)", r, errno);

    /* (3) Two LOCK_SH (shared) succeed simultaneously */
    r = flock(fd1, LOCK_SH);
    CHECK(r == 0, "flock(fd1, LOCK_SH) succeeds (rc=%d errno=%d)", r, errno);
    r = flock(fd2, LOCK_SH | LOCK_NB);
    CHECK(r == 0,
          "flock(fd2, LOCK_SH|LOCK_NB) coexists with fd1's LOCK_SH (rc=%d errno=%d)",
          r, errno);

    /* (4) LOCK_SH on fd1 blocks LOCK_EX on fd2 */
    (void)flock(fd2, LOCK_UN);
    /* fd1 still holds LOCK_SH */
    errno = 0;
    r = flock(fd2, LOCK_EX | LOCK_NB);
    CHECK(r == -1 && errno == EWOULDBLOCK,
          "fd1's LOCK_SH blocks flock(fd2, LOCK_EX|LOCK_NB) (rc=%d errno=%d)",
          r, errno);

    /* (5) After fd1 LOCK_UN, fd2 LOCK_EX|LOCK_NB succeeds */
    (void)flock(fd1, LOCK_UN);
    r = flock(fd2, LOCK_EX | LOCK_NB);
    CHECK(r == 0,
          "after fd1 LOCK_UN, fd2 LOCK_EX|LOCK_NB succeeds (rc=%d errno=%d)",
          r, errno);
    (void)flock(fd2, LOCK_UN);

    close(fd1);
    close(fd2);
    unlink(path);

    printf("\n=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("ALL TESTS PASSED\n");
        return 0;
    }
    printf("SOME TESTS FAILED\n");
    return 1;
}
