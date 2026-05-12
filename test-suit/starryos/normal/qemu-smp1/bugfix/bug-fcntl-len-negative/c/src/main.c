/*
 * bug-fcntl-len-negative: fcntl record locks must accept a negative
 * `l_len` and treat the request as covering [l_start + l_len, l_start)
 * (man 3p fcntl: "If l_len is negative, the area affected shall start at
 * l_start+l_len and end at l_start-1"). Linux fs/locks.c
 * (`flock_to_posix_lock` / `flock64_to_posix_lock`) implements that
 * normalization; rejecting negative l_len with EINVAL is a real bug
 * because programs do legitimately lock backwards from a known endpoint.
 *
 * StarryOS bug (pre-fix, reviewer-flagged):
 *   kernel/src/syscall/fs/lock.rs::flock_range returned EINVAL for any
 *   l_len < 0, so any negative-len SETLK / GETLK failed.
 *
 * Phases (single process, single fd; fd is opened twice so we have one
 * fd holding the WRLCK and a separate fd doing GETLK queries — POSIX
 * record locks per-process don't conflict with themselves on the same
 * fd, but a GETLK from a different fd of the same pid still reports
 * accurately because GETLK ignores same-owner entries):
 *
 *   1. SETLK F_WRLCK with l_start=10, l_len=-5 → range [5,10).
 *      Verify success.
 *   2. From a *different process* (fork), GETLK F_WRLCK on disjoint
 *      ranges built with negative l_len → reports F_UNLCK.
 *   3. From the child, GETLK on overlapping negative-len ranges →
 *      reports the lock; l_start/l_len match the held range.
 *   4. Child SETLK F_WRLCK on a known-conflicting negative-len range
 *      with non-blocking F_SETLK → EAGAIN.
 *   5. Parent UNLCK with a negative-len range that subsumes the held
 *      lock → lock disappears (verified by GETLK from child).
 *   6. SETLK with l_len = INT64_MIN or `l_start + l_len` < 0 → EINVAL.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static int passed = 0;
static int failed = 0;

#define CHECK(cond, fmt, ...) do {                                         \
    if (cond) { printf("  PASS: " fmt "\n", ##__VA_ARGS__); passed++; }    \
    else      { printf("  FAIL: " fmt "\n", ##__VA_ARGS__); failed++; }    \
} while (0)

static int do_setlk(int fd, int cmd, short l_type, off_t start, off_t len)
{
    struct flock fl;
    memset(&fl, 0, sizeof(fl));
    fl.l_type = l_type;
    fl.l_whence = SEEK_SET;
    fl.l_start = start;
    fl.l_len = len;
    return fcntl(fd, cmd, &fl);
}

static int do_getlk(int fd, short want, off_t start, off_t len,
                    struct flock *out)
{
    memset(out, 0, sizeof(*out));
    out->l_type = want;
    out->l_whence = SEEK_SET;
    out->l_start = start;
    out->l_len = len;
    return fcntl(fd, F_GETLK, out);
}

static void wbyte(int fd, char b) { (void)!write(fd, &b, 1); }
static char rbyte(int fd) { char b = 0; (void)!read(fd, &b, 1); return b; }

int main(void)
{
    printf("=== bug-fcntl-len-negative ===\n");

    const char *path = "/tmp/starry_bug_fcntl_len_negative";
    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) {
        printf("FAIL: open: %s\n", strerror(errno));
        return 1;
    }
    if (ftruncate(fd, 4096) != 0) {
        printf("FAIL: ftruncate: %s\n", strerror(errno));
        close(fd); return 1;
    }

    /* Phase 1: lock [5,10) via l_start=10, l_len=-5. */
    int r = do_setlk(fd, F_SETLK, F_WRLCK, 10, -5);
    CHECK(r == 0,
          "SETLK F_WRLCK l_start=10 l_len=-5 (range [5,10)) succeeds (rc=%d errno=%d)",
          r, errno);

    /* Phase 6 (early — same-pid, no fork needed): EINVAL for malformed
     * negative-len ranges. */
    errno = 0;
    int r_neg_overflow = do_setlk(fd, F_SETLK, F_WRLCK, 5, INT64_MIN);
    CHECK(r_neg_overflow == -1 && (errno == EINVAL),
          "SETLK l_start=5 l_len=INT64_MIN → EINVAL (rc=%d errno=%d)",
          r_neg_overflow, errno);
    errno = 0;
    /* Resolved start would be -1 (l_start=4 + l_len=-5 == -1). */
    int r_neg_below_zero = do_setlk(fd, F_SETLK, F_WRLCK, 4, -5);
    CHECK(r_neg_below_zero == -1 && errno == EINVAL,
          "SETLK l_start=4 l_len=-5 (resolves to start=-1) → EINVAL (rc=%d errno=%d)",
          r_neg_below_zero, errno);

    int p2c[2], c2p[2];
    if (pipe(p2c) != 0 || pipe(c2p) != 0) {
        printf("FAIL: pipe: %s\n", strerror(errno));
        close(fd); unlink(path); return 1;
    }

    pid_t cpid = fork();
    if (cpid < 0) { printf("FAIL: fork\n"); close(fd); return 1; }
    if (cpid == 0) {
        close(p2c[1]); close(c2p[0]);
        int cfd = open(path, O_RDWR);
        if (cfd < 0) _exit(30);

        /* Wait for parent to confirm the lock is held. */
        if (rbyte(p2c[0]) != 'L') _exit(31);

        struct flock out;
        /* Phase 2: GETLK on disjoint ranges built from negative l_len.
         * Range [0,5): l_start=5 l_len=-5. No overlap with [5,10). */
        if (do_getlk(cfd, F_WRLCK, 5, -5, &out) != 0) _exit(40);
        if (out.l_type != F_UNLCK) {
            fprintf(stderr,
                    "child: GETLK [0,5) reported l_type=%d, expected F_UNLCK\n",
                    out.l_type);
            _exit(41);
        }
        /* Range [10,15): l_start=15 l_len=-5. No overlap. */
        if (do_getlk(cfd, F_WRLCK, 15, -5, &out) != 0) _exit(42);
        if (out.l_type != F_UNLCK) _exit(43);

        /* Phase 3: GETLK on overlapping negative-len ranges → reports
         * the WRLCK held by parent. Range [9,12) via l_start=12, l_len=-3. */
        if (do_getlk(cfd, F_WRLCK, 12, -3, &out) != 0) _exit(50);
        if (out.l_type != F_WRLCK) {
            fprintf(stderr,
                    "child: GETLK [9,12) reported l_type=%d, expected F_WRLCK\n",
                    out.l_type);
            _exit(51);
        }
        if (out.l_start != 5 || out.l_len != 5) {
            fprintf(stderr,
                    "child: GETLK reported l_start=%lld l_len=%lld, expected 5/5\n",
                    (long long)out.l_start, (long long)out.l_len);
            _exit(52);
        }

        /* Phase 4: SETLK F_WRLCK on a conflicting negative-len range →
         * EAGAIN. Range [6,8) via l_start=8, l_len=-2. */
        errno = 0;
        int sr = do_setlk(cfd, F_SETLK, F_WRLCK, 8, -2);
        if (sr != -1 || (errno != EAGAIN && errno != EACCES)) {
            fprintf(stderr,
                    "child: SETLK [6,8) returned rc=%d errno=%d, expected -1/EAGAIN\n",
                    sr, errno);
            _exit(60);
        }

        /* Tell parent it's safe to unlock. */
        wbyte(c2p[1], 'U');
        /* Wait for parent to confirm release. */
        if (rbyte(p2c[0]) != 'R') _exit(70);

        /* Phase 5 verification: GETLK on the previously-held range →
         * F_UNLCK. */
        if (do_getlk(cfd, F_WRLCK, 12, -3, &out) != 0) _exit(80);
        if (out.l_type != F_UNLCK) {
            fprintf(stderr,
                    "child: post-release GETLK l_type=%d, expected F_UNLCK\n",
                    out.l_type);
            _exit(81);
        }

        close(cfd);
        _exit(0);
    }

    close(p2c[0]); close(c2p[1]);

    /* Tell child the parent's lock is in place. */
    wbyte(p2c[1], 'L');

    /* Wait until the child has finished its conflict checks. */
    if (rbyte(c2p[0]) != 'U') {
        printf("FAIL: child sync byte\n");
    }

    /* Phase 5: UNLCK with a negative-len range that fully covers
     * [5,10). l_start=12, l_len=-7 → resolves to [5,12). */
    int u = do_setlk(fd, F_SETLK, F_UNLCK, 12, -7);
    CHECK(u == 0,
          "UNLCK l_start=12 l_len=-7 (range [5,12), covers held lock) succeeds (rc=%d errno=%d)",
          u, errno);

    /* Tell child the lock is gone. */
    wbyte(p2c[1], 'R');

    int wstatus = 0;
    pid_t w = waitpid(cpid, &wstatus, 0);
    CHECK(w == cpid, "waitpid");
    CHECK(WIFEXITED(wstatus) && WEXITSTATUS(wstatus) == 0,
          "child exit status 0 (status=0x%x)", wstatus);

    close(fd);
    unlink(path);

    printf("=== bug-fcntl-len-negative: passed=%d failed=%d ===\n",
           passed, failed);
    return failed == 0 ? 0 : 1;
}
