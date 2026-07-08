/*
 * bug-fcntl-partial-wake: F_SETLKW waiters must be woken when the holder
 * releases *part* of a wider conflicting range, not just when the entry
 * disappears entirely.
 *
 * StarryOS bug (pre-fix, reviewer-flagged):
 *   kernel/src/syscall/fs/lock.rs::try_setlk_once used
 *   `entries.len() != before` as the "did anything change" proxy. The
 *   split case (one entry removed, tail re-inserted) keeps the length
 *   identical, so a waiter parked on the released sub-range never got
 *   woken. Repro:
 *     - A holds WRLCK [0, 100) on the file.
 *     - B calls F_SETLKW for [0, 50) — parks.
 *     - A unlocks [0, 50). Table now holds A:[50, 100); length still 1.
 *     - Without the fix, B never wakes.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

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

static long ms_since(const struct timespec *t0)
{
    struct timespec t1;
    clock_gettime(CLOCK_MONOTONIC, &t1);
    long sec = (long)(t1.tv_sec - t0->tv_sec);
    long ns = (long)(t1.tv_nsec - t0->tv_nsec);
    return sec * 1000 + ns / 1000000;
}

static void msleep(long ms)
{
    struct timespec ts;
    ts.tv_sec = ms / 1000;
    ts.tv_nsec = (ms % 1000) * 1000000;
    nanosleep(&ts, NULL);
}

static void wbyte(int fd, char b) { (void)!write(fd, &b, 1); }
static char rbyte(int fd) { char b = 0; (void)!read(fd, &b, 1); return b; }

int main(void)
{
    printf("=== bug-fcntl-partial-wake ===\n");
    const char *path = "/tmp/starry_bug_fcntl_partial_wake";

    int p2c[2], c2p[2];
    if (pipe(p2c) != 0 || pipe(c2p) != 0) {
        printf("  FAIL: pipe: %s\n", strerror(errno));
        return 1;
    }

    int pfd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (pfd < 0) { printf("  FAIL: open: %s\n", strerror(errno)); return 1; }
    if (ftruncate(pfd, 4096) != 0) {
        printf("  FAIL: ftruncate\n"); close(pfd); return 1;
    }

    /* Parent acquires WRLCK over [0, 100). */
    if (do_setlk(pfd, F_SETLK, F_WRLCK, 0, 100) != 0) {
        printf("  FAIL: parent acquire: %s\n", strerror(errno));
        close(pfd); return 1;
    }

    pid_t cpid = fork();
    if (cpid < 0) { printf("  FAIL: fork\n"); close(pfd); return 1; }
    if (cpid == 0) {
        close(p2c[1]); close(c2p[0]);
        int cfd = open(path, O_RDWR);
        if (cfd < 0) _exit(10);

        wbyte(c2p[1], 'R');

        struct timespec t0;
        clock_gettime(CLOCK_MONOTONIC, &t0);
        /* Park on the LEFT half [0, 50). The parent will release exactly
         * that sub-range; the right half [50, 100) stays locked. */
        if (do_setlk(cfd, F_SETLKW, F_WRLCK, 0, 50) != 0) {
            fprintf(stderr, "child: SETLKW: %s\n", strerror(errno));
            _exit(11);
        }
        long elapsed = ms_since(&t0);
        if (elapsed < 50) {
            fprintf(stderr,
                    "child: SETLKW returned in %ld ms (expected >=50)\n",
                    elapsed);
            _exit(12);
        }
        /* Sanity: we must not have stolen the right half. F_GETLK on
         * [50, 100) should report the parent's lock. */
        struct flock probe;
        memset(&probe, 0, sizeof(probe));
        probe.l_type = F_WRLCK;
        probe.l_whence = SEEK_SET;
        probe.l_start = 50;
        probe.l_len = 50;
        if (fcntl(cfd, F_GETLK, &probe) != 0) {
            fprintf(stderr, "child: GETLK: %s\n", strerror(errno));
            _exit(13);
        }
        if (probe.l_type != F_WRLCK || probe.l_start != 50 || probe.l_len != 50) {
            fprintf(stderr,
                    "child: right-half should still be parent-WRLCK [50,100), "
                    "got type=%d start=%lld len=%lld\n",
                    (int)probe.l_type,
                    (long long)probe.l_start, (long long)probe.l_len);
            _exit(14);
        }
        do_setlk(cfd, F_SETLK, F_UNLCK, 0, 50);
        close(cfd);
        _exit(0);
    }

    close(p2c[0]); close(c2p[1]);
    char ready = rbyte(c2p[0]);
    CHECK(ready == 'R', "child reached SETLKW");

    msleep(150);
    /* Partial unlock: release just [0, 50). The parent's table still
     * holds the right half [50, 100). The pre-fix bug skipped the wake
     * because entries.len() did not change. */
    if (do_setlk(pfd, F_SETLK, F_UNLCK, 0, 50) != 0) {
        printf("  FAIL: parent partial unlock: %s\n", strerror(errno));
    }

    int wstatus = 0;
    pid_t w = waitpid(cpid, &wstatus, 0);
    CHECK(w == cpid, "waitpid");
    CHECK(WIFEXITED(wstatus) && WEXITSTATUS(wstatus) == 0,
          "child woke and acquired [0,50) (status=0x%x)", wstatus);

    do_setlk(pfd, F_SETLK, F_UNLCK, 50, 50);
    close(pfd);
    close(p2c[1]); close(c2p[0]);
    unlink(path);

    printf("=== bug-fcntl-partial-wake: passed=%d failed=%d ===\n",
           passed, failed);
    return failed == 0 ? 0 : 1;
}
