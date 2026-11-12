/*
 * bug-fcntl-setlkw-blocks: F_SETLKW / F_OFD_SETLKW must actually block on
 * a conflicting lock and resume when the conflict clears, instead of
 * collapsing to the same EAGAIN return as F_SETLK.
 *
 * StarryOS bug (pre-fix, reviewer-flagged):
 *   kernel/src/syscall/fs/lock.rs accepted *SETLKW for ABI completeness
 *   but ignored the wait flag — every conflict returned WouldBlock right
 *   away, so any program that expected the kernel to suspend it (e.g. a
 *   coordinated-write daemon waiting for an exclusive byte-range lock)
 *   would spin or fail.
 *
 * Phases:
 *   1. F_SETLKW (POSIX) wakes after the conflicting lock is released
 *      (blocks for at least the parent's release delay).
 *   2. F_OFD_SETLKW wakes after the conflicting OFD lock is released
 *      (separate parent-held fd, separate `Arc<dyn FileLike>`).
 *   3. F_SETLKW returns EINTR when interrupted by a delivered signal
 *      with a handler installed (POSIX requirement).
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

/* OFD command numbers may be missing from older libc headers. They are
 * stable Linux ABI numbers (37/38/36) on every architecture we target. */
#ifndef F_OFD_GETLK
#define F_OFD_GETLK 36
#endif
#ifndef F_OFD_SETLK
#define F_OFD_SETLK 37
#endif
#ifndef F_OFD_SETLKW
#define F_OFD_SETLKW 38
#endif

static int passed = 0;
static int failed = 0;

#define CHECK(cond, fmt, ...) do {                                         \
    if (cond) { printf("  PASS: " fmt "\n", ##__VA_ARGS__); passed++; }    \
    else      { printf("  FAIL: " fmt "\n", ##__VA_ARGS__); failed++; }    \
} while (0)

static void sigusr1_handler(int sig) { (void)sig; /* just interrupt */ }

static int do_setlk_cmd(int fd, int cmd, short l_type,
                        off_t start, off_t len)
{
    struct flock fl;
    memset(&fl, 0, sizeof(fl));
    fl.l_type = l_type;
    fl.l_whence = SEEK_SET;
    fl.l_start = start;
    fl.l_len = len;
    return fcntl(fd, cmd, &fl);
}

/* Monotonic millisecond delta for verifying that we actually slept. */
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

/* Phase 1 + 2: parent locks a range, child blocks on *SETLKW for the same
 * range; parent waits, then releases; child must wake and acquire. We
 * verify both POSIX (F_SETLKW) and OFD (F_OFD_SETLKW). */
static int wait_for_release(const char *phase, int parent_cmd, int child_cmd,
                            const char *path)
{
    int p2c[2], c2p[2];
    if (pipe(p2c) != 0 || pipe(c2p) != 0) {
        printf("FAIL: %s: pipe: %s\n", phase, strerror(errno));
        return 1;
    }

    int pfd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (pfd < 0) {
        printf("FAIL: %s: open: %s\n", phase, strerror(errno));
        return 1;
    }
    if (ftruncate(pfd, 4096) != 0) {
        printf("FAIL: %s: ftruncate: %s\n", phase, strerror(errno));
        close(pfd); return 1;
    }

    /* Parent acquires WRLCK [0,100). For F_OFD_* the lock is owned by
     * this OFD (this fd); the child must open its own fd to a separate
     * OFD so the conflict is across OFDs, not across pids. */
    if (do_setlk_cmd(pfd, parent_cmd, F_WRLCK, 0, 100) != 0) {
        printf("FAIL: %s: parent acquire: %s\n", phase, strerror(errno));
        close(pfd); return 1;
    }

    pid_t cpid = fork();
    if (cpid < 0) {
        printf("FAIL: %s: fork: %s\n", phase, strerror(errno));
        close(pfd); return 1;
    }
    if (cpid == 0) {
        close(p2c[1]); close(c2p[0]);
        /* Open a fresh fd in the child — separate OFD, so OFD locks
         * conflict across the parent/child boundary the same way POSIX
         * locks do across pids. */
        int cfd = open(path, O_RDWR);
        if (cfd < 0) { _exit(10); }

        /* Tell the parent we're ready to block. */
        wbyte(c2p[1], 'R');

        struct timespec t0;
        clock_gettime(CLOCK_MONOTONIC, &t0);
        if (do_setlk_cmd(cfd, child_cmd, F_WRLCK, 0, 100) != 0) {
            fprintf(stderr, "child %s: SETLKW returned err: %s\n",
                    phase, strerror(errno));
            _exit(11);
        }
        long elapsed = ms_since(&t0);
        /* Parent will sleep ~150ms before releasing. Child must observe
         * a non-trivial wait (>=50ms). The previous bug returned
         * immediately, so a regression would land near 0ms here. */
        if (elapsed < 50) {
            fprintf(stderr,
                    "child %s: SETLKW returned in %ld ms (expected >=50)\n",
                    phase, elapsed);
            _exit(12);
        }
        /* Hand the lock back so we exit clean. */
        do_setlk_cmd(cfd, child_cmd, F_UNLCK, 0, 100);
        close(cfd);
        _exit(0);
    }

    close(p2c[0]); close(c2p[1]);
    /* Wait until the child is at the SETLKW call, then sleep a bit, then
     * release. */
    char ready = rbyte(c2p[0]);
    CHECK(ready == 'R', "%s: child reached SETLKW", phase);

    msleep(150);
    if (do_setlk_cmd(pfd, parent_cmd, F_UNLCK, 0, 100) != 0) {
        printf("FAIL: %s: parent release: %s\n", phase, strerror(errno));
    }

    int wstatus = 0;
    pid_t w = waitpid(cpid, &wstatus, 0);
    CHECK(w == cpid, "%s: waitpid", phase);
    CHECK(WIFEXITED(wstatus),
          "%s: child exited normally (status=0x%x)", phase, wstatus);
    CHECK(WIFEXITED(wstatus) && WEXITSTATUS(wstatus) == 0,
          "%s: child exit status 0", phase);

    close(pfd);
    close(p2c[1]); close(c2p[0]);
    return 0;
}

/* Phase 3: F_SETLKW interrupted by signal returns EINTR. */
static int interrupted_by_signal(const char *path)
{
    int p2c[2], c2p[2];
    if (pipe(p2c) != 0 || pipe(c2p) != 0) {
        printf("FAIL: eintr: pipe: %s\n", strerror(errno));
        return 1;
    }

    int pfd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (pfd < 0) { printf("FAIL: eintr: open\n"); return 1; }
    if (ftruncate(pfd, 4096) != 0) { close(pfd); return 1; }
    if (do_setlk_cmd(pfd, F_SETLK, F_WRLCK, 0, 100) != 0) {
        printf("FAIL: eintr: parent acquire: %s\n", strerror(errno));
        close(pfd); return 1;
    }

    pid_t cpid = fork();
    if (cpid < 0) { printf("FAIL: eintr: fork\n"); close(pfd); return 1; }
    if (cpid == 0) {
        close(p2c[1]); close(c2p[0]);
        struct sigaction sa;
        memset(&sa, 0, sizeof(sa));
        sa.sa_handler = sigusr1_handler;
        sigaction(SIGUSR1, &sa, NULL);

        int cfd = open(path, O_RDWR);
        if (cfd < 0) _exit(20);

        wbyte(c2p[1], 'R');
        errno = 0;
        int r = do_setlk_cmd(cfd, F_SETLKW, F_WRLCK, 0, 100);
        int saved_errno = errno;
        if (r == 0) {
            fprintf(stderr, "child eintr: SETLKW unexpectedly succeeded\n");
            _exit(21);
        }
        if (saved_errno != EINTR) {
            fprintf(stderr,
                    "child eintr: errno=%d (%s), expected EINTR\n",
                    saved_errno, strerror(saved_errno));
            _exit(22);
        }
        close(cfd);
        _exit(0);
    }

    close(p2c[0]); close(c2p[1]);
    char ready = rbyte(c2p[0]);
    CHECK(ready == 'R', "eintr: child reached SETLKW");
    /* Give the child time to actually enter the wait. */
    msleep(50);
    if (kill(cpid, SIGUSR1) != 0) {
        printf("FAIL: eintr: kill: %s\n", strerror(errno));
    }

    int wstatus = 0;
    pid_t w = waitpid(cpid, &wstatus, 0);
    CHECK(w == cpid, "eintr: waitpid");
    CHECK(WIFEXITED(wstatus) && WEXITSTATUS(wstatus) == 0,
          "eintr: child saw EINTR (status=0x%x)", wstatus);

    /* Parent still holds the lock — release for cleanup. */
    do_setlk_cmd(pfd, F_SETLK, F_UNLCK, 0, 100);
    close(pfd);
    close(p2c[1]); close(c2p[0]);
    return 0;
}

int main(void)
{
    printf("=== bug-fcntl-setlkw-blocks ===\n");

    const char *posix_path = "/tmp/starry_bug_setlkw_posix";
    const char *ofd_path   = "/tmp/starry_bug_setlkw_ofd";
    const char *eintr_path = "/tmp/starry_bug_setlkw_eintr";

    wait_for_release("posix", F_SETLK, F_SETLKW, posix_path);
    wait_for_release("ofd",   F_OFD_SETLK, F_OFD_SETLKW, ofd_path);
    interrupted_by_signal(eintr_path);

    unlink(posix_path);
    unlink(ofd_path);
    unlink(eintr_path);

    printf("=== bug-fcntl-setlkw-blocks: passed=%d failed=%d ===\n",
           passed, failed);
    return failed == 0 ? 0 : 1;
}
