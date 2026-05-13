/*
 * bug-flock-blocks: flock(2) without LOCK_NB must block on a conflicting
 * lock and wake when it is released, instead of collapsing to EWOULDBLOCK
 * the way the LOCK_NB form does.
 *
 * StarryOS bug (pre-fix, reviewer-flagged):
 *   kernel/src/syscall/fs/lock.rs::flock_op always returned WouldBlock on
 *   conflict and stripped LOCK_NB before matching, so flock(fd, LOCK_EX)
 *   without LOCK_NB would fail immediately — programs that rely on the
 *   call to suspend the caller (e.g. cooperative pid files, lock files
 *   used by `flock(1)`) would never get the lock.
 *
 * Phases:
 *   1. blocking LOCK_EX wakes after the parent's exclusive lock is
 *      released (must observe >=50 ms of sleep against the parent's
 *      ~150 ms hold).
 *   2. blocking LOCK_NB short-circuit still returns EWOULDBLOCK on
 *      conflict (regression guard for the non-blocking path).
 *   3. blocking LOCK_EX interrupted by a signal returns EINTR.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
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

static void sigusr1_handler(int sig) { (void)sig; /* just interrupt */ }

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

/* Phase 1: parent holds LOCK_EX, child blocks on flock(LOCK_EX); parent
 * releases after ~150 ms; child must wake and acquire. */
static void blocks_then_wakes(const char *path)
{
    int p2c[2], c2p[2];
    if (pipe(p2c) != 0 || pipe(c2p) != 0) {
        printf("  FAIL: blocks: pipe: %s\n", strerror(errno));
        failed++; return;
    }

    int pfd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (pfd < 0) { printf("  FAIL: blocks: open\n"); failed++; return; }
    if (flock(pfd, LOCK_EX) != 0) {
        printf("  FAIL: blocks: parent acquire: %s\n", strerror(errno));
        close(pfd); failed++; return;
    }

    pid_t cpid = fork();
    if (cpid < 0) {
        printf("  FAIL: blocks: fork\n");
        close(pfd); failed++; return;
    }
    if (cpid == 0) {
        close(p2c[1]); close(c2p[0]);
        /* Open a fresh fd (separate OFD) so flock conflicts. */
        int cfd = open(path, O_RDWR);
        if (cfd < 0) _exit(10);

        wbyte(c2p[1], 'R');

        struct timespec t0;
        clock_gettime(CLOCK_MONOTONIC, &t0);
        if (flock(cfd, LOCK_EX) != 0) {
            fprintf(stderr, "child blocks: flock: %s\n", strerror(errno));
            _exit(11);
        }
        long elapsed = ms_since(&t0);
        if (elapsed < 50) {
            fprintf(stderr,
                    "child blocks: flock returned in %ld ms (expected >=50)\n",
                    elapsed);
            _exit(12);
        }
        flock(cfd, LOCK_UN);
        close(cfd);
        _exit(0);
    }

    close(p2c[0]); close(c2p[1]);
    char ready = rbyte(c2p[0]);
    CHECK(ready == 'R', "blocks: child reached flock");

    msleep(150);
    if (flock(pfd, LOCK_UN) != 0) {
        printf("  FAIL: blocks: parent release: %s\n", strerror(errno));
    }

    int wstatus = 0;
    pid_t w = waitpid(cpid, &wstatus, 0);
    CHECK(w == cpid, "blocks: waitpid");
    CHECK(WIFEXITED(wstatus) && WEXITSTATUS(wstatus) == 0,
          "blocks: child acquired after wake (status=0x%x)", wstatus);

    close(pfd);
    close(p2c[1]); close(c2p[0]);
}

/* Phase 2: with LOCK_NB the call must return EWOULDBLOCK immediately. */
static void nb_short_circuits(const char *path)
{
    int pfd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (pfd < 0) { printf("  FAIL: nb: open\n"); failed++; return; }
    if (flock(pfd, LOCK_EX) != 0) {
        printf("  FAIL: nb: parent acquire: %s\n", strerror(errno));
        close(pfd); failed++; return;
    }

    pid_t cpid = fork();
    if (cpid < 0) {
        printf("  FAIL: nb: fork\n");
        close(pfd); failed++; return;
    }
    if (cpid == 0) {
        int cfd = open(path, O_RDWR);
        if (cfd < 0) _exit(20);
        struct timespec t0;
        clock_gettime(CLOCK_MONOTONIC, &t0);
        errno = 0;
        int r = flock(cfd, LOCK_EX | LOCK_NB);
        int saved = errno;
        long elapsed = ms_since(&t0);
        if (r == 0) {
            fprintf(stderr, "child nb: LOCK_NB unexpectedly succeeded\n");
            _exit(21);
        }
        if (saved != EWOULDBLOCK && saved != EAGAIN) {
            fprintf(stderr, "child nb: errno=%d (%s), expected EWOULDBLOCK\n",
                    saved, strerror(saved));
            _exit(22);
        }
        /* Should be effectively instantaneous (a regression to a blocking
         * impl would sit here forever; cap loosely at 50 ms). */
        if (elapsed > 50) {
            fprintf(stderr, "child nb: returned in %ld ms (expected <50)\n",
                    elapsed);
            _exit(23);
        }
        close(cfd);
        _exit(0);
    }

    int wstatus = 0;
    pid_t w = waitpid(cpid, &wstatus, 0);
    CHECK(w == cpid, "nb: waitpid");
    CHECK(WIFEXITED(wstatus) && WEXITSTATUS(wstatus) == 0,
          "nb: LOCK_NB short-circuits with EWOULDBLOCK (status=0x%x)",
          wstatus);

    flock(pfd, LOCK_UN);
    close(pfd);
}

/* Phase 3: a parked blocking flock returns EINTR when interrupted. */
static void interrupted_by_signal(const char *path)
{
    int p2c[2], c2p[2];
    if (pipe(p2c) != 0 || pipe(c2p) != 0) {
        printf("  FAIL: eintr: pipe\n"); failed++; return;
    }

    int pfd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (pfd < 0) { printf("  FAIL: eintr: open\n"); failed++; return; }
    if (flock(pfd, LOCK_EX) != 0) {
        printf("  FAIL: eintr: parent acquire: %s\n", strerror(errno));
        close(pfd); failed++; return;
    }

    pid_t cpid = fork();
    if (cpid < 0) {
        printf("  FAIL: eintr: fork\n"); close(pfd); failed++; return;
    }
    if (cpid == 0) {
        close(p2c[1]); close(c2p[0]);
        struct sigaction sa;
        memset(&sa, 0, sizeof(sa));
        sa.sa_handler = sigusr1_handler;
        sigaction(SIGUSR1, &sa, NULL);

        int cfd = open(path, O_RDWR);
        if (cfd < 0) _exit(30);
        wbyte(c2p[1], 'R');
        errno = 0;
        int r = flock(cfd, LOCK_EX);
        int saved = errno;
        if (r == 0) {
            fprintf(stderr, "child eintr: flock unexpectedly succeeded\n");
            _exit(31);
        }
        if (saved != EINTR) {
            fprintf(stderr, "child eintr: errno=%d (%s), expected EINTR\n",
                    saved, strerror(saved));
            _exit(32);
        }
        close(cfd);
        _exit(0);
    }

    close(p2c[0]); close(c2p[1]);
    char ready = rbyte(c2p[0]);
    CHECK(ready == 'R', "eintr: child reached flock");
    msleep(50);
    if (kill(cpid, SIGUSR1) != 0) {
        printf("  FAIL: eintr: kill: %s\n", strerror(errno));
    }

    int wstatus = 0;
    pid_t w = waitpid(cpid, &wstatus, 0);
    CHECK(w == cpid, "eintr: waitpid");
    CHECK(WIFEXITED(wstatus) && WEXITSTATUS(wstatus) == 0,
          "eintr: child saw EINTR (status=0x%x)", wstatus);

    flock(pfd, LOCK_UN);
    close(pfd);
    close(p2c[1]); close(c2p[0]);
}

int main(void)
{
    printf("=== bug-flock-blocks ===\n");

    const char *blocks_path = "/tmp/starry_bug_flock_blocks";
    const char *nb_path     = "/tmp/starry_bug_flock_nb";
    const char *eintr_path  = "/tmp/starry_bug_flock_eintr";

    blocks_then_wakes(blocks_path);
    nb_short_circuits(nb_path);
    interrupted_by_signal(eintr_path);

    unlink(blocks_path);
    unlink(nb_path);
    unlink(eintr_path);

    printf("=== bug-flock-blocks: passed=%d failed=%d ===\n", passed, failed);
    return failed == 0 ? 0 : 1;
}
