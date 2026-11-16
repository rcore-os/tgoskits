/* memfd_create + F_SEAL_* regression test.
 *
 * Covers seal enforcement implemented in StarryOS:
 *   - memfd_create("anon", MFD_ALLOW_SEALING) returns a valid fd
 *   - F_GET_SEALS reports 0 on a fresh memfd
 *   - F_ADD_SEALS sets bits readable via F_GET_SEALS
 *   - F_SEAL_SHRINK rejects ftruncate to a smaller size with EPERM
 *   - F_SEAL_GROW rejects ftruncate to a larger size with EPERM
 *   - F_SEAL_WRITE rejects mmap(PROT_WRITE | MAP_SHARED) with EPERM
 *   - F_SEAL_FUTURE_WRITE seals an already-populated memfd and rejects
 *     writes issued after sealing with EPERM
 *   - F_SEAL_SEAL blocks all subsequent F_ADD_SEALS with EPERM
 *   - Without MFD_ALLOW_SEALING, F_ADD_SEALS is rejected with EPERM
 *   - copy_file_range and pipe-to-memfd splice honor explicit output offsets
 *     and reject F_SEAL_WRITE destinations without publishing changes
 *
 * Note: enforcing F_SEAL_WRITE against pre-existing writable mappings
 * (post-mmap revoke) is deferred to a later kernel change and is not
 * exercised here.
 */

#define _GNU_SOURCE
#define _FILE_OFFSET_BITS 64
#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/types.h>
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
#ifndef F_SEAL_FUTURE_WRITE
#define F_SEAL_FUTURE_WRITE 0x0010
#endif
#ifndef MFD_CLOEXEC
#define MFD_CLOEXEC 0x0001
#endif
#ifndef MFD_ALLOW_SEALING
#define MFD_ALLOW_SEALING 0x0002
#endif

#ifndef SYS_copy_file_range
#ifdef __NR_copy_file_range
#define SYS_copy_file_range __NR_copy_file_range
#endif
#endif
#ifndef SYS_copy_file_range
#error "SYS_copy_file_range is not available on this target"
#endif

#ifndef SYS_splice
#ifdef __NR_splice
#define SYS_splice __NR_splice
#endif
#endif
#ifndef SYS_splice
#error "SYS_splice is not available on this target"
#endif

static int memfd_create_sys(const char *name, unsigned int flags) {
    return (int)syscall(SYS_memfd_create, name, flags);
}

static int memfd_fixture(const char *name, size_t size,
                         const char *initial, size_t initial_len) {
    int fd = memfd_create_sys(name, MFD_CLOEXEC | MFD_ALLOW_SEALING);
    if (fd < 0) {
        return -1;
    }
    if (ftruncate(fd, (off_t)size) != 0) {
        int saved_errno = errno;
        close(fd);
        errno = saved_errno;
        return -1;
    }
    if (initial_len != 0 &&
        pwrite(fd, initial, initial_len, 0) != (ssize_t)initial_len) {
        int saved_errno = errno;
        close(fd);
        errno = saved_errno;
        return -1;
    }
    return fd;
}

static int bytes_equal_at(int fd, const char *expected, size_t len, off_t offset) {
    char actual[64];
    if (len > sizeof(actual)) {
        return 0;
    }
    return pread(fd, actual, len, offset) == (ssize_t)len &&
           memcmp(actual, expected, len) == 0;
}

static long raw_copy_file_range(int fd_in, off_t *off_in, int fd_out,
                                off_t *off_out, size_t len) {
    return syscall(SYS_copy_file_range, fd_in, off_in, fd_out, off_out, len, 0);
}

static long raw_splice(int fd_in, off_t *off_in, int fd_out, off_t *off_out,
                       size_t len, unsigned int flags) {
    return syscall(SYS_splice, fd_in, off_in, fd_out, off_out, len, flags);
}

static void test_memfd_transfer_offsets(void) {
    enum { FIXTURE_SIZE = 64 };
    static const char payload[] = "memfd-transfer";
    const size_t payload_len = sizeof(payload) - 1;

    /* copy_file_range: memfd source and memfd explicit output offset. */
    int copy_src = memfd_fixture("copy-src", FIXTURE_SIZE, payload, payload_len);
    int copy_dst = memfd_fixture("copy-dst", FIXTURE_SIZE, NULL, 0);
    CHECK(copy_src >= 0 && copy_dst >= 0, "create memfd copy_file_range fixtures");
    if (copy_src >= 0 && copy_dst >= 0) {
        CHECK_RET(lseek(copy_src, 7, SEEK_SET), 7,
                  "set source cursor away from explicit copy offset");
        CHECK_RET(lseek(copy_dst, 3, SEEK_SET), 3,
                  "set destination cursor away from explicit copy offset");

        off_t off_in = 0;
        off_t off_out = 16;
        ssize_t copied = (ssize_t)raw_copy_file_range(
            copy_src, &off_in, copy_dst, &off_out, payload_len);
        CHECK(copied == (ssize_t)payload_len,
              "copy_file_range copies into memfd at explicit output offset");
        CHECK(off_in == (off_t)payload_len,
              "copy_file_range advances explicit input offset");
        CHECK(off_out == 16 + (off_t)payload_len,
              "copy_file_range advances explicit output offset");
        CHECK(lseek(copy_src, 0, SEEK_CUR) == 7,
              "copy_file_range preserves source implicit position");
        CHECK(lseek(copy_dst, 0, SEEK_CUR) == 3,
              "copy_file_range preserves destination implicit position");
        CHECK(bytes_equal_at(copy_dst, payload, payload_len, 16),
              "copy_file_range writes payload at explicit destination offset");
    }
    if (copy_src >= 0) {
        close(copy_src);
    }
    if (copy_dst >= 0) {
        close(copy_dst);
    }

    /* splice: real pipe source and memfd explicit output offset. */
    int pipefd[2] = {-1, -1};
    int splice_dst = memfd_fixture("splice-dst", FIXTURE_SIZE, NULL, 0);
    int pipe_result = pipe(pipefd);
    CHECK_RET(pipe_result, 0, "create pipe for memfd splice");
    CHECK(splice_dst >= 0, "create memfd splice destination");
    if (splice_dst >= 0 && pipe_result == 0) {
        ssize_t written = write(pipefd[1], payload, payload_len);
        CHECK(written == (ssize_t)payload_len, "fill pipe for memfd splice");
        if (written == (ssize_t)payload_len) {
            CHECK_RET(lseek(splice_dst, 5, SEEK_SET), 5,
                      "set splice destination implicit position");

            off_t off_out = 24;
            ssize_t spliced =
                (ssize_t)raw_splice(pipefd[0], NULL, splice_dst, &off_out, payload_len, 0);
            CHECK(spliced == (ssize_t)payload_len,
                  "splice copies from pipe into memfd at explicit output offset");
            CHECK(off_out == 24 + (off_t)payload_len,
                  "splice advances explicit memfd output offset");
            CHECK(lseek(splice_dst, 0, SEEK_CUR) == 5,
                  "splice preserves memfd implicit position");
            CHECK(bytes_equal_at(splice_dst, payload, payload_len, 24),
                  "splice writes payload at explicit memfd output offset");
        }
    }
    if (splice_dst >= 0) {
        close(splice_dst);
    }
    if (pipefd[0] >= 0) {
        close(pipefd[0]);
    }
    if (pipefd[1] >= 0) {
        close(pipefd[1]);
    }

    /* Sealed destinations reject both transfer implementations without
     * publishing offset or content changes. Pipe consumption is intentionally
     * not asserted because it is outside this regression's contract. */
    char protected_data[FIXTURE_SIZE];
    memset(protected_data, 'Q', sizeof(protected_data));
    int sealed_dst = memfd_fixture("sealed-dst", FIXTURE_SIZE,
                                   protected_data, sizeof(protected_data));
    int sealed_src = memfd_fixture("sealed-src", FIXTURE_SIZE,
                                   payload, payload_len);
    CHECK(sealed_dst >= 0 && sealed_src >= 0,
          "create fixtures for sealed transfer destinations");
    if (sealed_dst >= 0 && sealed_src >= 0 &&
        fcntl(sealed_dst, F_ADD_SEALS, F_SEAL_WRITE) == 0) {
        CHECK_RET(lseek(sealed_dst, 11, SEEK_SET), 11,
                  "set sealed destination implicit position");

        off_t off_in = 0;
        off_t off_out = 16;
        errno = 0;
        CHECK(raw_copy_file_range(sealed_src, &off_in, sealed_dst,
                                  &off_out, payload_len) == -1 &&
                  errno == EPERM,
              "copy_file_range rejects F_SEAL_WRITE memfd output with EPERM");
        CHECK(off_out == 16,
              "failed copy_file_range leaves memfd output offset unchanged");
        CHECK(lseek(sealed_dst, 0, SEEK_CUR) == 11,
              "failed copy_file_range preserves memfd implicit position");
        CHECK(bytes_equal_at(sealed_dst, protected_data, sizeof(protected_data), 0),
              "failed copy_file_range leaves sealed destination content unchanged");

        pipefd[0] = -1;
        pipefd[1] = -1;
        CHECK_RET(pipe(pipefd), 0, "create pipe for sealed memfd splice");
        if (pipefd[0] >= 0 && pipefd[1] >= 0) {
            ssize_t written = write(pipefd[1], payload, payload_len);
            CHECK(written == (ssize_t)payload_len,
                  "fill pipe for sealed memfd splice");
            if (written == (ssize_t)payload_len) {
                off_out = 16;
                errno = 0;
                CHECK(raw_splice(pipefd[0], NULL, sealed_dst, &off_out,
                                 payload_len, 0) == -1 &&
                          errno == EPERM,
                      "splice rejects F_SEAL_WRITE memfd output with EPERM");
                CHECK(off_out == 16,
                      "failed splice leaves memfd output offset unchanged");
                CHECK(lseek(sealed_dst, 0, SEEK_CUR) == 11,
                      "failed splice preserves memfd implicit position");
                CHECK(bytes_equal_at(sealed_dst, protected_data,
                                     sizeof(protected_data), 0),
                      "failed splice leaves sealed destination content unchanged");
            }
        }
        if (pipefd[0] >= 0) {
            close(pipefd[0]);
        }
        if (pipefd[1] >= 0) {
            close(pipefd[1]);
        }
    } else {
        CHECK(0, "seal transfer destination with F_SEAL_WRITE");
    }
    if (sealed_dst >= 0) {
        close(sealed_dst);
    }
    if (sealed_src >= 0) {
        close(sealed_src);
    }
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

        /* Non-zero write(2)/pwrite(2) under F_SEAL_WRITE fails with EPERM
         * (man: those calls fail with EPERM if this seal is set). */
        char wbuf[16];
        memset(wbuf, 'Z', sizeof(wbuf));
        errno = 0;
        CHECK(pwrite(wfd, wbuf, sizeof(wbuf), 0) == -1 && errno == EPERM,
              "non-zero pwrite rejected with EPERM under F_SEAL_WRITE");
        errno = 0;
        CHECK(write(wfd, wbuf, sizeof(wbuf)) == -1 && errno == EPERM,
              "non-zero write rejected with EPERM under F_SEAL_WRITE");

        close(wfd);
    }

    /* --- F_ADD_SEALS(F_SEAL_WRITE) fails with EBUSY while a writable shared
     * mapping is live; succeeds once unmapped (man: EBUSY if any writable,
     * shared mapping exists). --------------------------------------------- */
    int bfd = memfd_create_sys("wbusy", MFD_CLOEXEC | MFD_ALLOW_SEALING);
    CHECK(bfd >= 0, "memfd_create write-busy fd");
    if (bfd >= 0) {
        CHECK_RET(ftruncate(bfd, 4096), 0, "write-busy fd ftruncate");
        void *pmw = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, bfd, 0);
        CHECK(pmw != MAP_FAILED, "mmap(PROT_WRITE|MAP_SHARED) succeeds pre-seal");
        if (pmw != MAP_FAILED) {
            CHECK_ERR(fcntl(bfd, F_ADD_SEALS, F_SEAL_WRITE), EBUSY,
                      "F_ADD_SEALS(F_SEAL_WRITE) EBUSY with live writable shared mapping");
            CHECK_RET(munmap(pmw, 4096), 0, "munmap writable shared mapping");
            CHECK_RET(fcntl(bfd, F_ADD_SEALS, F_SEAL_WRITE), 0,
                      "F_ADD_SEALS(F_SEAL_WRITE) succeeds after munmap");
            int sbusy = fcntl(bfd, F_GET_SEALS, 0);
            CHECK((sbusy & F_SEAL_WRITE) != 0,
                  "F_GET_SEALS reports F_SEAL_WRITE after munmap+add");
        }
        close(bfd);
    }

    /* --- F_SEAL_FUTURE_WRITE blocks writes issued after sealing ------- */
    /* Unlike F_SEAL_WRITE, F_SEAL_FUTURE_WRITE can seal an already-populated
     * memfd and only rejects future writes; Chromium/Firefox seal a
     * read-only snapshot for sandboxed children this way. */
    int ffd = memfd_create_sys("fwseal", MFD_CLOEXEC | MFD_ALLOW_SEALING);
    CHECK(ffd >= 0, "memfd_create future-write-seal fd");
    if (ffd >= 0) {
        CHECK_RET(ftruncate(ffd, 4096), 0, "future-write-seal fd ftruncate");
        CHECK(write(ffd, "AB", 2) == 2, "write allowed before F_SEAL_FUTURE_WRITE");
        CHECK_RET(fcntl(ffd, F_ADD_SEALS, F_SEAL_FUTURE_WRITE), 0,
                  "F_ADD_SEALS F_SEAL_FUTURE_WRITE");
        seals = fcntl(ffd, F_GET_SEALS, 0);
        CHECK((seals & F_SEAL_FUTURE_WRITE) != 0,
              "F_GET_SEALS reports F_SEAL_FUTURE_WRITE");

        errno = 0;
        CHECK(pwrite(ffd, "z", 1, 0) == -1 && errno == EPERM,
              "pwrite rejected with EPERM after F_SEAL_FUTURE_WRITE");

        /* Zero-length write still returns 0 under FUTURE_WRITE, matching
         * Linux, which never synthesizes EPERM for a count==0 write. */
        char zb[1];
        errno = 0;
        CHECK(pwrite(ffd, zb, 0, 0) == 0 && errno == 0,
              "zero-length pwrite returns 0 under F_SEAL_FUTURE_WRITE");

        close(ffd);
    }

    /* --- F_ADD_SEALS with an unrecognized seal bit -> EINVAL (man:
     * "arg includes unrecognized sealing bits"). 0x8000 is outside
     * F_ALL_SEALS on every kernel variant. ---------------------------- */
    int ifd = memfd_create_sys("inval", MFD_CLOEXEC | MFD_ALLOW_SEALING);
    CHECK(ifd >= 0, "memfd_create invalid-seal fd");
    if (ifd >= 0) {
        CHECK_ERR(fcntl(ifd, F_ADD_SEALS, 0x8000), EINVAL,
                  "F_ADD_SEALS unrecognized bit 0x8000 rejected with EINVAL");
        CHECK_ERR(fcntl(ifd, F_ADD_SEALS, F_SEAL_SHRINK | 0x8000), EINVAL,
                  "F_ADD_SEALS valid|unrecognized bit rejected with EINVAL");
        int sinval = fcntl(ifd, F_GET_SEALS, 0);
        CHECK(sinval == 0, "rejected F_ADD_SEALS installed no seals");
        close(ifd);
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

    /* --- seal fcntls on non-sealable fds return EINVAL (man: on other
     * filesystems all seal fcntl operations return EINVAL). ------------- */
    int rf = open("/tmp/memfd_seal_nonmemfd.tmp", O_RDWR | O_CREAT | O_TRUNC, 0600);
    CHECK(rf >= 0, "open regular file for non-memfd seal check");
    if (rf >= 0) {
        CHECK_ERR(fcntl(rf, F_GET_SEALS, 0), EINVAL,
                  "F_GET_SEALS on regular file rejected with EINVAL");
        CHECK_ERR(fcntl(rf, F_ADD_SEALS, F_SEAL_SHRINK), EINVAL,
                  "F_ADD_SEALS on regular file rejected with EINVAL");
        close(rf);
        unlink("/tmp/memfd_seal_nonmemfd.tmp");
    }
    int npfd[2];
    CHECK_RET(pipe(npfd), 0, "pipe for non-memfd seal check");
    CHECK_ERR(fcntl(npfd[0], F_GET_SEALS, 0), EINVAL,
              "F_GET_SEALS on pipe rejected with EINVAL");
    CHECK_ERR(fcntl(npfd[1], F_ADD_SEALS, F_SEAL_WRITE), EINVAL,
              "F_ADD_SEALS on pipe rejected with EINVAL");
    close(npfd[0]);
    close(npfd[1]);

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

    test_memfd_transfer_offsets();

    TEST_DONE();
}
