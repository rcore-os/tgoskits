/*
 * bug-fcntl-posix-exit-release: POSIX advisory locks must be released
 * when their owning process exits, even without an explicit F_UNLCK.
 *
 * Linux releases all locks held by a pid via fl_release_private hooks
 * driven by the last close on the underlying inode. StarryOS tracks
 * POSIX locks by pid, so the release must run from the process-exit
 * path (kernel/src/task/ops.rs calling release_pid_locks).
 *
 * Without that hook, a child that forks → F_SETLK → exits permanently
 * pins a record in FCNTL_LOCKS and blocks every subsequent acquirer
 * on the same inode/range — even after the original holder is gone.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
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
    printf("=== bug-fcntl-posix-exit-release ===\n");

    const char *path = "/tmp/starry_bug_fcntl_exit";
    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) {
        printf("FAIL: open: %s\n", strerror(errno));
        return 1;
    }

    pid_t child = fork();
    if (child < 0) {
        printf("FAIL: fork: %s\n", strerror(errno));
        close(fd); unlink(path); return 1;
    }
    if (child == 0) {
        /* Child: take the lock and exit WITHOUT explicit F_UNLCK. */
        int r = posix_lock(fd, F_WRLCK, 0, 100);
        if (r != 0) { _exit(2); }
        _exit(0);
    }

    /* Parent: reap child, then try to take the same range. */
    int wstatus = 0;
    if (waitpid(child, &wstatus, 0) < 0) {
        printf("FAIL: waitpid: %s\n", strerror(errno));
        close(fd); unlink(path); return 1;
    }
    CHECK(WIFEXITED(wstatus) && WEXITSTATUS(wstatus) == 0,
          "child exited cleanly after taking the lock (status=0x%x)", wstatus);

    /* (1) Parent acquires the same range — must succeed because the
     *     child's exit released its POSIX lock. */
    errno = 0;
    int r = posix_lock(fd, F_WRLCK, 0, 100);
    CHECK(r == 0,
          "parent F_SETLK on [0,100) after child exit succeeds (rc=%d errno=%d)",
          r, errno);

    /* (2) F_GETLK on the same range must report it as free in the
     *     "no other holder" sense — query as F_WRLCK; we already hold
     *     the WRLCK so the other-holder lookup must report F_UNLCK. */
    struct flock g = {
        .l_type = F_WRLCK,
        .l_whence = SEEK_SET,
        .l_start = 0,
        .l_len = 100,
    };
    r = fcntl(fd, F_GETLK, &g);
    CHECK(r == 0 && g.l_type == F_UNLCK,
          "F_GETLK reports no other holder after child exit (rc=%d l_type=%d)",
          r, (int)g.l_type);

    /* Cleanup */
    (void)posix_lock(fd, F_UNLCK, 0, 100);
    close(fd);
    unlink(path);

    printf("\n=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) { printf("ALL TESTS PASSED\n"); return 0; }
    printf("SOME TESTS FAILED\n");
    return 1;
}
