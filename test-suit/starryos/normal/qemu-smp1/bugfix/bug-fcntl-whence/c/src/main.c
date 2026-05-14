/*
 * bug-fcntl-whence: fcntl record locks must accept SEEK_SET, SEEK_CUR and
 * SEEK_END for `struct flock.l_whence`, as POSIX/Linux do.
 *
 * StarryOS bug (pre-fix, reviewer-flagged):
 *   kernel/src/syscall/fs/lock.rs::fcntl_setlk / fcntl_getlk rejected any
 *   whence other than SEEK_SET with EINVAL, breaking the standard
 *   "lock relative to current offset" and "lock relative to file end"
 *   idioms.
 *
 * We exercise the resolver with F_OFD_SETLK / F_OFD_GETLK so the holder
 * and the probe live on different owners (separate OFDs) within the same
 * process; F_SETLK/F_GETLK with two fds in the same pid would otherwise
 * see "same owner" and report F_UNLCK regardless.
 *
 * Phases:
 *   1. SEEK_CUR resolves against the fd's current cursor.
 *      Lseek to 100, F_OFD_SETLK F_WRLCK whence=SEEK_CUR start=0 len=50
 *      installs [100, 150). Peer F_OFD_GETLK must see it.
 *   2. SEEK_END resolves against the file size. With size 1000,
 *      whence=SEEK_END start=-100 len=200 → installs [900, 1100).
 *      Peer F_OFD_GETLK must see [900, 1100) reported.
 *   3. An unknown whence value still returns EINVAL.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
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

static int do_setlk_whence(int fd, int cmd, short type, short whence,
                           off_t start, off_t len)
{
    struct flock fl;
    memset(&fl, 0, sizeof(fl));
    fl.l_type = type;
    fl.l_whence = whence;
    fl.l_start = start;
    fl.l_len = len;
    /* F_OFD_* require l_pid=0 (already zero from memset). */
    return fcntl(fd, cmd, &fl);
}

static int do_ofd_getlk_abs(int peer_fd, off_t start, off_t len,
                            struct flock *out)
{
    memset(out, 0, sizeof(*out));
    out->l_type = F_WRLCK;
    out->l_whence = SEEK_SET;
    out->l_start = start;
    out->l_len = len;
    return fcntl(peer_fd, F_OFD_GETLK, out);
}

static void phase_seek_cur(const char *path)
{
    int holder = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    int peer   = open(path, O_RDWR);
    if (holder < 0 || peer < 0) {
        printf("  FAIL: cur: open: %s\n", strerror(errno));
        if (holder >= 0) close(holder);
        if (peer >= 0) close(peer);
        failed++; return;
    }
    if (ftruncate(holder, 4096) != 0) {
        printf("  FAIL: cur: ftruncate\n");
        close(holder); close(peer); failed++; return;
    }
    if (lseek(holder, 100, SEEK_SET) != 100) {
        printf("  FAIL: cur: lseek\n");
        close(holder); close(peer); failed++; return;
    }
    /* SEEK_CUR start=0 len=50 → [100, 150) */
    if (do_setlk_whence(holder, F_OFD_SETLK, F_WRLCK, SEEK_CUR, 0, 50) != 0) {
        printf("  FAIL: cur: OFD_SETLK SEEK_CUR: %s\n", strerror(errno));
        close(holder); close(peer); failed++; return;
    }
    struct flock probe;
    if (do_ofd_getlk_abs(peer, 100, 50, &probe) != 0) {
        printf("  FAIL: cur: OFD_GETLK [100,150): %s\n", strerror(errno));
        do_setlk_whence(holder, F_OFD_SETLK, F_UNLCK, SEEK_SET, 100, 50);
        close(holder); close(peer); failed++; return;
    }
    CHECK(probe.l_type == F_WRLCK,
          "cur: GETLK [100,150) reports WRLCK (got %d)", (int)probe.l_type);
    CHECK(probe.l_start == 100 && probe.l_len == 50,
          "cur: GETLK reports [%lld,+%lld) (want [100,+50))",
          (long long)probe.l_start, (long long)probe.l_len);

    /* Range just before is free. */
    struct flock probe2;
    if (do_ofd_getlk_abs(peer, 0, 100, &probe2) != 0) {
        printf("  FAIL: cur: OFD_GETLK [0,100): %s\n", strerror(errno));
        failed++;
    } else {
        CHECK(probe2.l_type == F_UNLCK,
              "cur: OFD_GETLK [0,100) is F_UNLCK (got %d)",
              (int)probe2.l_type);
    }

    do_setlk_whence(holder, F_OFD_SETLK, F_UNLCK, SEEK_SET, 100, 50);
    close(holder); close(peer);
}

static void phase_seek_end(const char *path)
{
    int holder = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    int peer   = open(path, O_RDWR);
    if (holder < 0 || peer < 0) {
        printf("  FAIL: end: open: %s\n", strerror(errno));
        if (holder >= 0) close(holder);
        if (peer >= 0) close(peer);
        failed++; return;
    }
    if (ftruncate(holder, 1000) != 0) {
        printf("  FAIL: end: ftruncate\n");
        close(holder); close(peer); failed++; return;
    }
    /* SEEK_END start=-100 len=200 → [900, 1100). */
    if (do_setlk_whence(holder, F_OFD_SETLK, F_WRLCK, SEEK_END, -100, 200) != 0) {
        printf("  FAIL: end: OFD_SETLK SEEK_END: %s\n", strerror(errno));
        close(holder); close(peer); failed++; return;
    }
    struct flock probe;
    if (do_ofd_getlk_abs(peer, 900, 200, &probe) != 0) {
        printf("  FAIL: end: OFD_GETLK [900,1100): %s\n", strerror(errno));
        do_setlk_whence(holder, F_OFD_SETLK, F_UNLCK, SEEK_SET, 900, 200);
        close(holder); close(peer); failed++; return;
    }
    CHECK(probe.l_type == F_WRLCK,
          "end: GETLK [900,1100) reports WRLCK (got %d)", (int)probe.l_type);
    CHECK(probe.l_start == 900 && probe.l_len == 200,
          "end: GETLK reports [%lld,+%lld) (want [900,+200))",
          (long long)probe.l_start, (long long)probe.l_len);

    do_setlk_whence(holder, F_OFD_SETLK, F_UNLCK, SEEK_SET, 900, 200);
    close(holder); close(peer);
}

static void phase_bad_whence(const char *path)
{
    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) { printf("  FAIL: bad: open\n"); failed++; return; }
    errno = 0;
    int r = do_setlk_whence(fd, F_OFD_SETLK, F_WRLCK, /*whence=*/99, 0, 10);
    int saved = errno;
    CHECK(r != 0 && saved == EINVAL,
          "bad: OFD_SETLK whence=99 → EINVAL (r=%d errno=%d)", r, saved);
    close(fd);
}

int main(void)
{
    printf("=== bug-fcntl-whence ===\n");
    const char *path = "/tmp/starry_bug_fcntl_whence";

    phase_seek_cur(path);
    phase_seek_end(path);
    phase_bad_whence(path);

    unlink(path);

    printf("=== bug-fcntl-whence: passed=%d failed=%d ===\n", passed, failed);
    return failed == 0 ? 0 : 1;
}
