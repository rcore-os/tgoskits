/*
 * bug-fcntl-ofd-lock: Verify Open File Description (OFD) advisory locks via
 * fcntl(F_OFD_SETLK / F_OFD_GETLK). Unlike POSIX locks, OFD locks are
 * associated with the open file description, so two independent open()s
 * within the same process produce two independent OFDs and a write lock on
 * one must conflict with a write lock on the other.
 *
 * StarryOS bug: kernel/src/syscall/fs/fd_ops.rs:246-251 stubs out
 *     F_OFD_SETLK | F_OFD_SETLKW => Ok(0)
 *     F_OFD_GETLK                => sets l_type=F_UNLCK and returns 0
 * so the kernel reports every contended OFD lock as successfully acquired.
 *
 * No fork required: OFD lock independence between two open()s in the same
 * process is the canonical OFD vs POSIX distinguisher.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

/* Some libc headers may omit OFD constants; provide them. */
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

#define CHECK(cond, fmt, ...) do {                                  \
    if (cond) { printf("  PASS: " fmt "\n", ##__VA_ARGS__); passed++; } \
    else      { printf("  FAIL: " fmt "\n", ##__VA_ARGS__); failed++; } \
} while (0)

static int ofd_setlk(int fd, short l_type, off_t start, off_t len) {
    struct flock fl;
    memset(&fl, 0, sizeof(fl));
    fl.l_type = l_type;
    fl.l_whence = SEEK_SET;
    fl.l_start = start;
    fl.l_len = len;
    fl.l_pid = 0; /* must be 0 for OFD requests */
    return fcntl(fd, F_OFD_SETLK, &fl);
}

int main(void) {
    printf("=== bug-fcntl-ofd-lock ===\n");

    const char *path = "/tmp/starry_bug_fcntl_ofd_lock";
    int fd1 = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd1 < 0) {
        printf("FAIL: open fd1: %s\n", strerror(errno));
        return 1;
    }
    if (ftruncate(fd1, 4096) != 0) {
        printf("FAIL: ftruncate: %s\n", strerror(errno));
        close(fd1); unlink(path); return 1;
    }
    int fd2 = open(path, O_RDWR);
    if (fd2 < 0) {
        printf("FAIL: open fd2: %s\n", strerror(errno));
        close(fd1); unlink(path); return 1;
    }

    /* (1) fd1 acquires F_WRLCK [0..100) — should succeed */
    int r = ofd_setlk(fd1, F_WRLCK, 0, 100);
    CHECK(r == 0,
          "fd1 F_OFD_SETLK F_WRLCK [0..100) succeeds (rc=%d errno=%d)",
          r, errno);

    /* (2) fd2 F_WRLCK on same range — different OFD, must conflict */
    errno = 0;
    r = ofd_setlk(fd2, F_WRLCK, 0, 100);
    CHECK(r == -1 && (errno == EAGAIN || errno == EACCES),
          "fd2 F_OFD_SETLK F_WRLCK [0..100) returns EAGAIN (rc=%d errno=%d)",
          r, errno);

    /* (3) fd2 F_OFD_GETLK reports the conflict */
    struct flock fl;
    memset(&fl, 0, sizeof(fl));
    fl.l_type = F_WRLCK;
    fl.l_whence = SEEK_SET;
    fl.l_start = 0;
    fl.l_len = 100;
    fl.l_pid = 0;
    r = fcntl(fd2, F_OFD_GETLK, &fl);
    CHECK(r == 0 && fl.l_type == F_WRLCK,
          "fd2 F_OFD_GETLK [0..100) reports F_WRLCK (rc=%d l_type=%d)",
          r, (int)fl.l_type);

    /* (4) Non-overlapping range on fd2 succeeds */
    r = ofd_setlk(fd2, F_WRLCK, 200, 100);
    CHECK(r == 0,
          "fd2 F_OFD_SETLK F_WRLCK [200..300) (non-overlap) succeeds (rc=%d errno=%d)",
          r, errno);
    if (r == 0) (void)ofd_setlk(fd2, F_UNLCK, 200, 100);

    /* (5) dup(fd1) and close the dup — fd1's OFD lock must remain active */
    int fd1d = dup(fd1);
    if (fd1d >= 0) {
        close(fd1d);
        errno = 0;
        r = ofd_setlk(fd2, F_WRLCK, 0, 100);
        CHECK(r == -1 && (errno == EAGAIN || errno == EACCES),
              "after close(dup(fd1)), fd1's OFD lock remains (rc=%d errno=%d)",
              r, errno);
    } else {
        printf("  SKIP: dup(fd1) failed: %s\n", strerror(errno));
    }

    /* (6) close(fd1) → OFD destroyed, lock released, fd2 can acquire */
    close(fd1);
    errno = 0;
    r = ofd_setlk(fd2, F_WRLCK, 0, 100);
    CHECK(r == 0,
          "after close(fd1), fd2 F_OFD_SETLK F_WRLCK [0..100) succeeds (rc=%d errno=%d)",
          r, errno);
    if (r == 0) (void)ofd_setlk(fd2, F_UNLCK, 0, 100);

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
