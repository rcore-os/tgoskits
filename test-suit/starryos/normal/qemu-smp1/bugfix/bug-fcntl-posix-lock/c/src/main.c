/*
 * bug-fcntl-posix-lock: Verify POSIX advisory file locks via fcntl(F_SETLK,
 * F_GETLK) honor cross-process exclusion as defined by Linux fcntl(2).
 *
 * StarryOS bug: kernel/src/syscall/fs/fd_ops.rs:245-251 stubs out
 *     F_SETLK | F_SETLKW => Ok(0)
 *     F_GETLK            => sets l_type=F_UNLCK and returns 0
 * so concurrent processes silently believe they all hold exclusive locks.
 *
 * Phases (parent and child fork-synchronized via two pipes):
 *   A. Child F_SETLK on a range parent already write-locked → EAGAIN/EACCES.
 *   B. Child F_GETLK on that range → l_type=F_WRLCK, l_pid=parent_pid.
 *   C. After parent F_UNLCK, child F_SETLK on the range succeeds.
 *   D. Both processes hold F_RDLCK on the same range simultaneously.
 *   E. Parent F_WRLCK conflicts with child F_RDLCK on the same range.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static int passed = 0;
static int failed = 0;

#define CHECK(cond, fmt, ...) do {                                  \
    if (cond) { printf("  PASS: " fmt "\n", ##__VA_ARGS__); passed++; } \
    else      { printf("  FAIL: " fmt "\n", ##__VA_ARGS__); failed++; } \
} while (0)

static int do_setlk(int fd, short l_type, off_t start, off_t len) {
    struct flock fl;
    memset(&fl, 0, sizeof(fl));
    fl.l_type = l_type;
    fl.l_whence = SEEK_SET;
    fl.l_start = start;
    fl.l_len = len;
    return fcntl(fd, F_SETLK, &fl);
}

static int do_getlk(int fd, short want_type, off_t start, off_t len,
                    struct flock *out) {
    memset(out, 0, sizeof(*out));
    out->l_type = want_type;
    out->l_whence = SEEK_SET;
    out->l_start = start;
    out->l_len = len;
    return fcntl(fd, F_GETLK, out);
}

static void wbyte(int fd, char b) { (void)!write(fd, &b, 1); }
static char rbyte(int fd) { char b = 0; (void)!read(fd, &b, 1); return b; }

int main(void) {
    printf("=== bug-fcntl-posix-lock ===\n");

    const char *path = "/tmp/starry_bug_fcntl_posix_lock";
    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) {
        printf("FAIL: open(%s): %s\n", path, strerror(errno));
        return 1;
    }
    if (ftruncate(fd, 4096) != 0) {
        printf("FAIL: ftruncate: %s\n", strerror(errno));
        close(fd); unlink(path); return 1;
    }

    int p2c[2], c2p[2];
    if (pipe(p2c) != 0 || pipe(c2p) != 0) {
        printf("FAIL: pipe: %s\n", strerror(errno));
        close(fd); unlink(path); return 1;
    }

    /* Parent acquires WRLCK [0..100) before fork so it inherits in child? No —
     * POSIX locks are NOT inherited across fork (man fcntl). The parent's
     * locks remain associated with the parent only. */
    if (do_setlk(fd, F_WRLCK, 0, 100) != 0) {
        printf("FAIL: parent F_SETLK F_WRLCK [0..100): %s\n", strerror(errno));
        close(fd); unlink(path); return 1;
    }
    pid_t parent_pid = getpid();

    pid_t pid = fork();
    if (pid < 0) {
        printf("FAIL: fork: %s\n", strerror(errno));
        close(fd); unlink(path); return 1;
    }

    if (pid == 0) {
        /* ---------- child ---------- */
        close(p2c[1]); close(c2p[0]);
        int cfd = open(path, O_RDWR);
        if (cfd < 0) _exit(11);

        /* A: contended F_SETLK should return EAGAIN/EACCES */
        (void)rbyte(p2c[0]);
        errno = 0;
        int r = do_setlk(cfd, F_WRLCK, 0, 100);
        wbyte(c2p[1], (r == -1 && (errno == EAGAIN || errno == EACCES)) ? 'Y' : 'N');

        /* B: F_GETLK reports parent's WRLCK */
        (void)rbyte(p2c[0]);
        struct flock fl;
        int g = do_getlk(cfd, F_WRLCK, 0, 100, &fl);
        wbyte(c2p[1], (g == 0 && fl.l_type == F_WRLCK && fl.l_pid == (pid_t)getppid()) ? 'Y' : 'N');

        /* C: parent unlocked, child can take WRLCK */
        (void)rbyte(p2c[0]);
        int r4 = do_setlk(cfd, F_WRLCK, 0, 100);
        wbyte(c2p[1], r4 == 0 ? 'Y' : 'N');
        if (r4 == 0) (void)do_setlk(cfd, F_UNLCK, 0, 100);

        /* D: shared RDLCK on same range from both processes */
        (void)rbyte(p2c[0]);
        int r5 = do_setlk(cfd, F_RDLCK, 200, 100);
        wbyte(c2p[1], r5 == 0 ? 'Y' : 'N');
        (void)rbyte(p2c[0]); /* parent says: release */
        (void)do_setlk(cfd, F_UNLCK, 200, 100);
        wbyte(c2p[1], 'D');

        /* E: child RDLCK conflicts with parent's WRLCK */
        (void)rbyte(p2c[0]);
        errno = 0;
        int r6 = do_setlk(cfd, F_RDLCK, 400, 100);
        wbyte(c2p[1], (r6 == -1 && (errno == EAGAIN || errno == EACCES)) ? 'Y' : 'N');

        close(cfd);
        _exit(0);
    }

    /* ---------- parent ---------- */
    close(p2c[0]); close(c2p[1]);
    char rep;
    (void)parent_pid;

    /* A */
    wbyte(p2c[1], 'A');
    rep = rbyte(c2p[0]);
    CHECK(rep == 'Y', "child F_SETLK on contended range returns EAGAIN/EACCES");

    /* B */
    wbyte(p2c[1], 'B');
    rep = rbyte(c2p[0]);
    CHECK(rep == 'Y', "child F_GETLK reports F_WRLCK held by parent (l_pid==parent)");

    /* C */
    if (do_setlk(fd, F_UNLCK, 0, 100) != 0) {
        printf("FAIL: parent F_UNLCK [0..100): %s\n", strerror(errno));
        failed++;
    }
    wbyte(p2c[1], 'C');
    rep = rbyte(c2p[0]);
    CHECK(rep == 'Y', "child F_SETLK F_WRLCK [0..100) succeeds after parent F_UNLCK");

    /* D: parent RDLCK, signal child to also RDLCK */
    int pd = do_setlk(fd, F_RDLCK, 200, 100);
    if (pd != 0) {
        printf("FAIL: parent F_RDLCK [200..300): %s\n", strerror(errno));
        failed++;
    }
    wbyte(p2c[1], 'D');
    rep = rbyte(c2p[0]);
    CHECK(pd == 0 && rep == 'Y', "two processes hold F_RDLCK on the same range simultaneously");
    wbyte(p2c[1], 'd');             /* tell child to release */
    rep = rbyte(c2p[0]);            /* expect 'D' */
    (void)do_setlk(fd, F_UNLCK, 200, 100);

    /* E: parent WRLCK [400..500) conflicts with child RDLCK */
    int pe = do_setlk(fd, F_WRLCK, 400, 100);
    if (pe != 0) {
        printf("FAIL: parent F_WRLCK [400..500): %s\n", strerror(errno));
        failed++;
    }
    wbyte(p2c[1], 'E');
    rep = rbyte(c2p[0]);
    CHECK(pe == 0 && rep == 'Y', "child F_RDLCK conflicts with parent's F_WRLCK on same range");
    (void)do_setlk(fd, F_UNLCK, 400, 100);

    int status = 0;
    waitpid(pid, &status, 0);

    close(fd);
    unlink(path);
    close(p2c[1]); close(c2p[0]);

    printf("\n=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("ALL TESTS PASSED\n");
        return 0;
    }
    printf("SOME TESTS FAILED\n");
    return 1;
}
