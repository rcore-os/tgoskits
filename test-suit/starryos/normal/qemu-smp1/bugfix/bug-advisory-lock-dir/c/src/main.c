/*
 * bug-advisory-lock-dir: advisory locks must be allowed on directory fds.
 *
 * StarryOS bug (pre-fix, reviewer-flagged):
 *   `FileLike::inode_key` defaulted to `None` and only regular files
 *   overrode it, so `Directory` would fail `lockable()` with EBADF.
 *   Linux permits both `fcntl(F_SETLK)` reads and `flock(LOCK_SH/LOCK_EX)`
 *   on `open(..., O_DIRECTORY)` fds; programs that pid-file the parent
 *   directory of a runtime (and various process supervisors) rely on it.
 *
 * The F_GETLK probe uses F_OFD_GETLK so the holder and peer have distinct
 * owners (different OFDs) inside the same process — POSIX F_GETLK with
 * two fds in one pid sees "same owner" and would report F_UNLCK.
 *
 * Phases:
 *   1. flock(dirfd, LOCK_SH) succeeds, LOCK_UN releases.
 *   2. flock(dirfd, LOCK_EX) succeeds even on an O_RDONLY directory
 *      (flock doesn't check the fd's access mode).
 *   3. fcntl(dirfd, F_OFD_SETLK, F_RDLCK) succeeds; peer F_OFD_GETLK
 *      sees it.
 *   4. fcntl(dirfd, F_SETLK, F_WRLCK) returns EBADF on an O_RDONLY
 *      directory (mode check is enforced for record locks).
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

#ifndef F_OFD_GETLK
#define F_OFD_GETLK 36
#endif
#ifndef F_OFD_SETLK
#define F_OFD_SETLK 37
#endif

static int passed = 0;
static int failed = 0;

#define CHECK(cond, fmt, ...) do {                                         \
    if (cond) { printf("  PASS: " fmt "\n", ##__VA_ARGS__); passed++; }    \
    else      { printf("  FAIL: " fmt "\n", ##__VA_ARGS__); failed++; }    \
} while (0)

static int do_setlk(int fd, int cmd, short type, off_t start, off_t len)
{
    struct flock fl;
    memset(&fl, 0, sizeof(fl));
    fl.l_type = type;
    fl.l_whence = SEEK_SET;
    fl.l_start = start;
    fl.l_len = len;
    return fcntl(fd, cmd, &fl);
}

int main(void)
{
    printf("=== bug-advisory-lock-dir ===\n");
    const char *dir = "/tmp/starry_bug_lock_dir";

    /* Best-effort cleanup of a prior run. */
    rmdir(dir);
    if (mkdir(dir, 0755) != 0 && errno != EEXIST) {
        printf("  FAIL: mkdir: %s\n", strerror(errno));
        return 1;
    }
    int dirfd = open(dir, O_RDONLY | O_DIRECTORY);
    if (dirfd < 0) {
        printf("  FAIL: open(O_DIRECTORY): %s\n", strerror(errno));
        rmdir(dir);
        return 1;
    }

    /* 1. flock LOCK_SH then LOCK_UN. */
    int r = flock(dirfd, LOCK_SH);
    CHECK(r == 0, "flock(dirfd, LOCK_SH) succeeds (r=%d errno=%d)",
          r, r == 0 ? 0 : errno);
    if (r == 0) {
        CHECK(flock(dirfd, LOCK_UN) == 0, "flock(dirfd, LOCK_UN) succeeds");
    }

    /* 2. flock LOCK_EX (doesn't require write mode). */
    r = flock(dirfd, LOCK_EX);
    CHECK(r == 0, "flock(dirfd, LOCK_EX) succeeds on O_RDONLY dir "
                  "(r=%d errno=%d)", r, r == 0 ? 0 : errno);
    if (r == 0) {
        CHECK(flock(dirfd, LOCK_UN) == 0, "flock(dirfd, LOCK_UN) again");
    }

    /* 3. F_OFD_SETLK F_RDLCK + peer F_OFD_GETLK. (OFD so the same-pid
     *    peer fd has a *different* owner and the probe is observable.) */
    r = do_setlk(dirfd, F_OFD_SETLK, F_RDLCK, 0, 100);
    CHECK(r == 0, "fcntl(dirfd, F_OFD_SETLK, F_RDLCK) succeeds "
                  "(r=%d errno=%d)", r, r == 0 ? 0 : errno);

    int peer = open(dir, O_RDONLY | O_DIRECTORY);
    if (peer >= 0) {
        struct flock probe;
        memset(&probe, 0, sizeof(probe));
        probe.l_type = F_WRLCK;
        probe.l_whence = SEEK_SET;
        probe.l_start = 0;
        probe.l_len = 100;
        if (fcntl(peer, F_OFD_GETLK, &probe) == 0) {
            CHECK(probe.l_type == F_RDLCK,
                  "peer F_OFD_GETLK reports F_RDLCK on dir (got %d)",
                  (int)probe.l_type);
        } else {
            printf("  FAIL: peer F_OFD_GETLK: %s\n", strerror(errno));
            failed++;
        }
        close(peer);
    } else {
        printf("  FAIL: peer open: %s\n", strerror(errno));
        failed++;
    }
    if (r == 0) {
        do_setlk(dirfd, F_OFD_SETLK, F_UNLCK, 0, 100);
    }

    /* 4. F_WRLCK on an O_RDONLY dir → EBADF. */
    errno = 0;
    r = do_setlk(dirfd, F_SETLK, F_WRLCK, 0, 100);
    int saved = errno;
    CHECK(r != 0 && saved == EBADF,
          "fcntl(dirfd, F_SETLK, F_WRLCK) on O_RDONLY dir → EBADF "
          "(r=%d errno=%d)", r, saved);

    close(dirfd);
    rmdir(dir);

    printf("=== bug-advisory-lock-dir: passed=%d failed=%d ===\n",
           passed, failed);
    return failed == 0 ? 0 : 1;
}
