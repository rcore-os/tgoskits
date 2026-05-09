#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <unistd.h>

#ifndef __NR_fchmodat2
#define __NR_fchmodat2 452
#endif

#ifndef __NR_fchmodat
#error "__NR_fchmodat is required for this test"
#endif

static int test_passed = 0;
static int test_failed = 0;

#define TEST_START(name) \
    do { \
        printf("[TEST] %s\n", (name)); \
        test_passed = 0; \
        test_failed = 0; \
    } while (0)

#define CHECK(cond, msg) \
    do { \
        if (!(cond)) { \
            printf("  [FAIL] %s (errno=%d %s)\n", (msg), errno, strerror(errno)); \
            test_failed++; \
        } else { \
            printf("  [OK] %s\n", (msg)); \
            test_passed++; \
        } \
    } while (0)

#define TEST_DONE() \
    do { \
        printf("\n=== result: %d passed, %d failed ===\n", test_passed, test_failed); \
        if (test_failed == 0) { \
            printf("TEST PASSED\n"); \
        } \
        return (test_failed == 0) ? EXIT_SUCCESS : EXIT_FAILURE; \
    } while (0)

static int raw_fchmodat2(int dirfd, const char *path, mode_t mode, int flags)
{
    return (int)syscall(__NR_fchmodat2, dirfd, path, mode, flags);
}

static int raw_fchmodat_with_extra_arg(int dirfd, const char *path, mode_t mode, unsigned long extra)
{
    return (int)syscall(__NR_fchmodat, dirfd, path, mode, extra);
}

static int create_file_with_mode(const char *path, mode_t mode)
{
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (fd < 0) {
        return -1;
    }
    if (fchmod(fd, mode) != 0) {
        int saved_errno = errno;
        close(fd);
        errno = saved_errno;
        return -1;
    }
    return close(fd);
}

static int path_mode(const char *path, mode_t *mode)
{
    struct stat st;
    if (stat(path, &st) != 0) {
        return -1;
    }
    *mode = st.st_mode & 07777;
    return 0;
}

static int check_mode(const char *path, mode_t expected)
{
    mode_t actual = 0;
    errno = 0;
    if (path_mode(path, &actual) != 0) {
        return 0;
    }
    if (actual != expected) {
        errno = 0;
        printf("    mode mismatch: expected %04o actual %04o\n", expected, actual);
        return 0;
    }
    return 1;
}

int main(void)
{
    const char *path = "/tmp/bug_fchmodat2_flags_file";
    int fd;

    TEST_START("fchmodat2: reject invalid flags and keep legacy fchmodat flagless");

    unlink(path);

    CHECK(create_file_with_mode(path, 0600) == 0, "create regular file mode 0600");

    errno = 0;
    CHECK(raw_fchmodat2(AT_FDCWD, path, 0644, 0) == 0,
          "fchmodat2(flags=0) succeeds");
    CHECK(check_mode(path, 0644), "fchmodat2(flags=0) changes mode to 0644");

    errno = 0;
    CHECK(raw_fchmodat2(AT_FDCWD, path, 0600, AT_REMOVEDIR) == -1 && errno == EINVAL,
          "fchmodat2 rejects AT_REMOVEDIR with EINVAL");
    CHECK(check_mode(path, 0644), "invalid fchmodat2 flags do not change mode");

    fd = open(path, O_RDONLY);
    CHECK(fd >= 0, "open file for AT_EMPTY_PATH smoke");
    if (fd >= 0) {
        errno = 0;
        CHECK(raw_fchmodat2(fd, "", 0600, AT_EMPTY_PATH) == 0,
              "fchmodat2(AT_EMPTY_PATH) changes fd target");
        close(fd);
        CHECK(check_mode(path, 0600), "AT_EMPTY_PATH result mode is 0600");
    }

    errno = 0;
    CHECK(raw_fchmodat_with_extra_arg(AT_FDCWD, path, 0666, AT_REMOVEDIR) == 0,
          "legacy fchmodat ignores a fourth raw syscall argument");
    CHECK(check_mode(path, 0666), "legacy fchmodat changes mode to 0666");

    unlink(path);

    TEST_DONE();
}
