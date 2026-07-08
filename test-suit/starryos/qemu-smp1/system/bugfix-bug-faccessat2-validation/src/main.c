/*
 * bug-faccessat2-validation.c -- faccessat2 mode/flag validation
 *
 * Linux rejects access mode bits outside R_OK/W_OK/X_OK and flag bits outside
 * AT_EACCESS/AT_EMPTY_PATH/AT_SYMLINK_NOFOLLOW with EINVAL before treating the
 * path as a successful existence check. This keeps callers from silently
 * accepting misspelled access checks.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SYS_faccessat2
#define SYS_faccessat2 439
#endif

#ifndef AT_EMPTY_PATH
#define AT_EMPTY_PATH 0x1000
#endif

#ifndef AT_EACCESS
#define AT_EACCESS 0x200
#endif

#define INVALID_ACCESS_MODE 0x8
#define INVALID_ACCESS_FLAG 0x80000000u

static int test_passed;
static int test_failed;

#define TEST_START(name)                                                      \
    do {                                                                      \
        printf("[TEST] %s\n", (name));                                        \
        test_passed = 0;                                                      \
        test_failed = 0;                                                      \
    } while (0)

#define CHECK(cond, msg)                                                      \
    do {                                                                      \
        if (cond) {                                                           \
            printf("  [OK] %s\n", (msg));                                    \
            test_passed++;                                                    \
        } else {                                                              \
            printf("  [FAIL] %s (errno=%d %s)\n", (msg), errno,              \
                   strerror(errno));                                          \
            test_failed++;                                                    \
        }                                                                     \
    } while (0)

#define TEST_DONE()                                                           \
    do {                                                                      \
        printf("\n=== result: %d passed, %d failed ===\n", test_passed,       \
               test_failed);                                                  \
        if (test_failed == 0) {                                                \
            printf("TEST PASSED\n");                                         \
        }                                                                     \
        return test_failed == 0 ? EXIT_SUCCESS : EXIT_FAILURE;                \
    } while (0)

static long raw_faccessat2(int dirfd, const char *path, int mode,
                           unsigned int flags)
{
    return syscall(SYS_faccessat2, dirfd, path, mode, flags);
}

static long raw_faccessat_legacy(int dirfd, const char *path, int mode,
                                 unsigned int ignored_flags)
{
    return syscall(SYS_faccessat, dirfd, path, mode, ignored_flags);
}

static void expect_ok(const char *label, int dirfd, const char *path, int mode,
                      unsigned int flags)
{
    errno = 0;
    long rc = raw_faccessat2(dirfd, path, mode, flags);
    CHECK(rc == 0, label);
}

static void expect_errno(const char *label, int dirfd, const char *path,
                         int mode, unsigned int flags, int expected)
{
    errno = 0;
    long rc = raw_faccessat2(dirfd, path, mode, flags);
    CHECK(rc == -1 && errno == expected, label);
}

int main(void)
{
    TEST_START("faccessat2 validates mode and flags");

    const char *file_path = "/tmp/bug_faccessat2_validation_file";
    const char *link_path = "/tmp/bug_faccessat2_validation_link";
    const char *dangling_path = "/tmp/bug_faccessat2_validation_dangling";

    unlink(file_path);
    unlink(link_path);
    unlink(dangling_path);

    int fd = open(file_path, O_CREAT | O_RDWR | O_TRUNC, 0644);
    CHECK(fd >= 0, "create regular file");
    if (fd >= 0) {
        CHECK(write(fd, "x", 1) == 1, "write regular file");
        close(fd);
    }

    CHECK(symlink(file_path, link_path) == 0, "create symlink to file");
    CHECK(symlink("/tmp/bug_faccessat2_validation_missing", dangling_path) == 0,
          "create dangling symlink");

    expect_ok("F_OK on existing file succeeds", AT_FDCWD, file_path, F_OK, 0);
    expect_ok("R_OK|W_OK on existing file succeeds", AT_FDCWD, file_path,
              R_OK | W_OK, 0);
    expect_ok("AT_EACCESS accepted as a valid flag", AT_FDCWD, file_path, R_OK,
              AT_EACCESS);

    errno = 0;
    CHECK(raw_faccessat_legacy(AT_FDCWD, file_path, F_OK, INVALID_ACCESS_FLAG) == 0,
          "legacy faccessat ignores the fourth syscall register");

    expect_errno("invalid access mode bit returns EINVAL", AT_FDCWD, file_path,
                 INVALID_ACCESS_MODE, 0, EINVAL);
    expect_errno("mixed valid and invalid access mode returns EINVAL", AT_FDCWD,
                 file_path, R_OK | INVALID_ACCESS_MODE, 0, EINVAL);
    expect_errno("invalid flag bit returns EINVAL", AT_FDCWD, file_path, F_OK,
                 INVALID_ACCESS_FLAG, EINVAL);
    expect_errno("AT_EMPTY_PATH plus invalid flag returns EINVAL", AT_FDCWD, "",
                 F_OK, AT_EMPTY_PATH | INVALID_ACCESS_FLAG, EINVAL);

    int fd2 = open(file_path, O_RDONLY);
    CHECK(fd2 >= 0, "open regular file for AT_EMPTY_PATH");
    if (fd2 >= 0) {
        expect_ok("AT_EMPTY_PATH with empty path succeeds", fd2, "", F_OK,
                  AT_EMPTY_PATH);
        expect_errno("invalid mode with AT_EMPTY_PATH returns EINVAL", fd2, "",
                     INVALID_ACCESS_MODE, AT_EMPTY_PATH, EINVAL);
        close(fd2);
    }

    expect_errno("dangling symlink follows target and returns ENOENT", AT_FDCWD,
                 dangling_path, F_OK, 0, ENOENT);
    expect_ok("AT_SYMLINK_NOFOLLOW checks dangling symlink itself", AT_FDCWD,
              dangling_path, F_OK, AT_SYMLINK_NOFOLLOW);

    unlink(file_path);
    unlink(link_path);
    unlink(dangling_path);

    TEST_DONE();
}
