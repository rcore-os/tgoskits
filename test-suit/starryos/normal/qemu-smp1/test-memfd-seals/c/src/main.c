/* memfd_create + F_SEAL_* regression test.
 *
 * Covers seal enforcement implemented in StarryOS:
 *   - memfd_create("anon", MFD_ALLOW_SEALING) returns a valid fd
 *   - F_GET_SEALS reports 0 on a fresh memfd
 *   - F_ADD_SEALS sets bits readable via F_GET_SEALS
 *   - F_SEAL_SHRINK rejects ftruncate to a smaller size with EPERM
 *   - F_SEAL_GROW rejects ftruncate to a larger size with EPERM
 *   - F_SEAL_WRITE rejects mmap(PROT_WRITE | MAP_SHARED) with EPERM
 *   - F_SEAL_SEAL blocks all subsequent F_ADD_SEALS with EPERM
 *   - Without MFD_ALLOW_SEALING, F_ADD_SEALS is rejected with EPERM
 *
 * Note: enforcing F_SEAL_WRITE against pre-existing writable mappings
 * (post-mmap revoke) is deferred to a later kernel change and is not
 * exercised here.
 */

#define _GNU_SOURCE
#include "test_framework.h"

#include <fcntl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef POSIX_FADV_NORMAL
#define POSIX_FADV_NORMAL 0
#endif

#ifndef F_ADD_SEALS
#define F_ADD_SEALS 1033
#endif
#ifndef F_GET_SEALS
#define F_GET_SEALS 1034
#endif
#ifndef F_SEAL_SEAL
#define F_SEAL_SEAL    0x0001
#define F_SEAL_SHRINK  0x0002
#define F_SEAL_GROW    0x0004
#define F_SEAL_WRITE   0x0008
#endif
#ifndef MFD_CLOEXEC
#define MFD_CLOEXEC 0x0001
#endif
#ifndef MFD_ALLOW_SEALING
#define MFD_ALLOW_SEALING 0x0002
#endif

static int memfd_create_sys(const char *name, unsigned int flags) {
    return (int)syscall(SYS_memfd_create, name, flags);
}

int main(void) {
    TEST_START("memfd_seals");

    /* --- create + initial state -------------------------------------- */
    int fd = memfd_create_sys("anon", MFD_CLOEXEC | MFD_ALLOW_SEALING);
    CHECK(fd >= 0, "memfd_create returns valid fd");
    if (fd < 0) {
        TEST_DONE();
    }

    CHECK_RET(ftruncate(fd, 4096), 0, "ftruncate initial size 4096");

    int seals = fcntl(fd, F_GET_SEALS, 0);
    CHECK(seals == 0, "F_GET_SEALS == 0 on fresh memfd");

    /* --- F_SEAL_SHRINK ------------------------------------------------ */
    CHECK_RET(fcntl(fd, F_ADD_SEALS, F_SEAL_SHRINK), 0, "F_ADD_SEALS F_SEAL_SHRINK");
    seals = fcntl(fd, F_GET_SEALS, 0);
    CHECK((seals & F_SEAL_SHRINK) != 0, "F_GET_SEALS reports F_SEAL_SHRINK");

    CHECK_ERR(ftruncate(fd, 1024), EPERM, "ftruncate shrink rejected with EPERM");
    CHECK_RET(ftruncate(fd, 4096), 0, "ftruncate same-size still allowed");

    /* --- F_SEAL_GROW -------------------------------------------------- */
    int gfd = memfd_create_sys("grow", MFD_CLOEXEC | MFD_ALLOW_SEALING);
    CHECK(gfd >= 0, "memfd_create grow fd");
    if (gfd >= 0) {
        CHECK_RET(ftruncate(gfd, 4096), 0, "grow fd ftruncate initial 4096");
        CHECK_RET(fcntl(gfd, F_ADD_SEALS, F_SEAL_GROW), 0, "F_ADD_SEALS F_SEAL_GROW");
        CHECK_ERR(ftruncate(gfd, 8192), EPERM, "ftruncate grow rejected with EPERM");
        CHECK_RET(ftruncate(gfd, 4096), 0, "ftruncate same-size still allowed under GROW");

        /* F_SEAL_GROW pwrite Linux semantics: cross-EOF writes
         * short-write the bytes that fit before EOF, while writes
         * starting at or past EOF return -1 EPERM. */
        char buf[64];
        memset(buf, 'A', sizeof(buf));
        ssize_t pwn = pwrite(gfd, buf, sizeof(buf), 4090);
        CHECK(pwn == 6, "pwrite cross-EOF returns partial 6 bytes under F_SEAL_GROW");
        errno = 0;
        pwn = pwrite(gfd, buf, sizeof(buf), 4096);
        CHECK(pwn == -1 && errno == EPERM,
              "pwrite at EOF rejected with EPERM under F_SEAL_GROW");
        errno = 0;
        pwn = pwrite(gfd, buf, sizeof(buf), 8192);
        CHECK(pwn == -1 && errno == EPERM,
              "pwrite past EOF rejected with EPERM under F_SEAL_GROW");

        /* pwrite that stays within current size is still allowed. */
        pwn = pwrite(gfd, buf, sizeof(buf), 0);
        CHECK(pwn == (ssize_t)sizeof(buf),
              "in-bounds pwrite still allowed under F_SEAL_GROW");

        /* Sequential write(2) at the cursor must follow the same
         * Linux semantics: cross-EOF short-writes the in-range bytes,
         * at/past-EOF returns -1 EPERM. Exercises the cursor-aware
         * code path. */
        CHECK_RET(lseek(gfd, 4090, SEEK_SET), 4090,
                  "lseek to 4090 for cross-EOF write");
        ssize_t wn = write(gfd, buf, sizeof(buf));
        CHECK(wn == 6, "write cross-EOF returns partial 6 under F_SEAL_GROW");
        /* Cursor must have advanced by 6, sitting at EOF now. */
        off_t pos = lseek(gfd, 0, SEEK_CUR);
        CHECK(pos == 4096, "write cross-EOF advanced cursor to EOF");
        errno = 0;
        wn = write(gfd, buf, sizeof(buf));
        CHECK(wn == -1 && errno == EPERM,
              "write at EOF rejected with EPERM under F_SEAL_GROW");
        /* And the cursor should not have moved beyond EOF. */
        pos = lseek(gfd, 0, SEEK_CUR);
        CHECK(pos == 4096, "rejected write left cursor at EOF");

        /* fallocate that would extend the file past EOF is forbidden
         * under F_SEAL_GROW; an in-range fallocate is still allowed. */
        errno = 0;
        int frc = fallocate(gfd, 0, 0, 8192);
        CHECK(frc == -1 && errno == EPERM,
              "fallocate growing past EOF rejected with EPERM under F_SEAL_GROW");
        errno = 0;
        frc = fallocate(gfd, 0, 0, 4096);
        CHECK(frc == 0, "fallocate within current size still allowed under F_SEAL_GROW");

        /* Zero-length writes succeed unconditionally on Linux under
         * any seal, including at and past EOF — verified against
         * memfd_create + F_ADD_SEALS(F_SEAL_GROW) on a stock host. */
        errno = 0;
        pwn = pwrite(gfd, buf, 0, 4096);
        CHECK(pwn == 0 && errno == 0,
              "zero-length pwrite at EOF returns 0 under F_SEAL_GROW");
        errno = 0;
        pwn = pwrite(gfd, buf, 0, 8192);
        CHECK(pwn == 0 && errno == 0,
              "zero-length pwrite past EOF returns 0 under F_SEAL_GROW");
        errno = 0;
        ssize_t zwn = write(gfd, buf, 0);
        CHECK(zwn == 0 && errno == 0,
              "zero-length write at EOF returns 0 under F_SEAL_GROW");

        close(gfd);
    }

    /* --- F_SEAL_WRITE blocks mmap(PROT_WRITE | MAP_SHARED) ----------- */
    int wfd = memfd_create_sys("wseal", MFD_CLOEXEC | MFD_ALLOW_SEALING);
    CHECK(wfd >= 0, "memfd_create write-seal fd");
    if (wfd >= 0) {
        CHECK_RET(ftruncate(wfd, 4096), 0, "write-seal fd ftruncate");
        CHECK_RET(fcntl(wfd, F_ADD_SEALS, F_SEAL_WRITE), 0, "F_ADD_SEALS F_SEAL_WRITE");

        errno = 0;
        void *pw = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, wfd, 0);
        CHECK(pw == MAP_FAILED && errno == EPERM,
              "mmap(PROT_WRITE|MAP_SHARED) rejected with EPERM after F_SEAL_WRITE");
        if (pw != MAP_FAILED) munmap(pw, 4096);

        /* read-only shared mapping should still succeed */
        void *pr = mmap(NULL, 4096, PROT_READ, MAP_SHARED, wfd, 0);
        CHECK(pr != MAP_FAILED, "mmap(PROT_READ|MAP_SHARED) still allowed under F_SEAL_WRITE");
        if (pr != MAP_FAILED) munmap(pr, 4096);

        /* Zero-length write(2)/pwrite(2) under F_SEAL_WRITE must
         * still return 0 — Linux does not synthesize EPERM for a
         * count==0 write, even on a fully-sealed fd. */
        char zbuf[1];
        errno = 0;
        ssize_t zpw = pwrite(wfd, zbuf, 0, 0);
        CHECK(zpw == 0 && errno == 0,
              "zero-length pwrite returns 0 under F_SEAL_WRITE");
        errno = 0;
        ssize_t zwn2 = write(wfd, zbuf, 0);
        CHECK(zwn2 == 0 && errno == 0,
              "zero-length write returns 0 under F_SEAL_WRITE");

        close(wfd);
    }

    /* --- F_SEAL_SEAL blocks further F_ADD_SEALS ---------------------- */
    CHECK_RET(fcntl(fd, F_ADD_SEALS, F_SEAL_SEAL), 0, "F_ADD_SEALS F_SEAL_SEAL");
    CHECK_ERR(fcntl(fd, F_ADD_SEALS, F_SEAL_GROW), EPERM,
              "F_ADD_SEALS rejected after F_SEAL_SEAL");
    close(fd);

    /* --- without MFD_ALLOW_SEALING, F_ADD_SEALS must fail ------------ */
    int nfd = memfd_create_sys("noseal", MFD_CLOEXEC);
    CHECK(nfd >= 0, "memfd_create without MFD_ALLOW_SEALING");
    if (nfd >= 0) {
        CHECK_ERR(fcntl(nfd, F_ADD_SEALS, F_SEAL_SHRINK), EPERM,
                  "F_ADD_SEALS denied without MFD_ALLOW_SEALING");
        close(nfd);
    }

    /* --- fsync / fdatasync / fadvise64 on memfd ----------------------- */
    int sfd = memfd_create_sys("sync", MFD_CLOEXEC);
    CHECK(sfd >= 0, "memfd_create for fsync coverage");
    if (sfd >= 0) {
        CHECK_RET(ftruncate(sfd, 4096), 0, "ftruncate for fsync coverage");
        CHECK_RET(fsync(sfd), 0, "fsync(memfd) accepted (Linux: success)");
        CHECK_RET(fdatasync(sfd), 0, "fdatasync(memfd) accepted");
        CHECK_RET(posix_fadvise(sfd, 0, 0, POSIX_FADV_NORMAL), 0,
                  "posix_fadvise(memfd) accepted");
        close(sfd);
    }

    TEST_DONE();
}
