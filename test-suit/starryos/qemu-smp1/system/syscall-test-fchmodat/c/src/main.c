#define _GNU_SOURCE

#include "test_framework.h"

#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <unistd.h>

#ifndef SYS_fchmodat
#define SYS_fchmodat 53
#endif

static char base[PATH_MAX];
static int dirfd = -1;

#ifndef SYS_fchmodat2
#define SYS_fchmodat2 452
#endif

static int do_fchmodat(int dfd, const char *path, mode_t mode)
{
    return (int)syscall(SYS_fchmodat, dfd, path, mode);
}

static int do_fchmodat2(int dfd, const char *path, mode_t mode, int flags)
{
    return (int)syscall(SYS_fchmodat2, dfd, path, mode, flags);
}

static void join_path(char *out, size_t out_len, const char *dir,
                      const char *name)
{
    int ret = snprintf(out, out_len, "%s/%s", dir, name);
    ASSERT_TRUE(ret > 0 && (size_t)ret < out_len, "build absolute path");
}

static void assert_mode_at(int dfd, const char *path, mode_t expected,
                           const char *msg)
{
    struct stat st;
    ASSERT_OK(fstatat(dfd, path, &st, 0), msg);
    mode_t actual = st.st_mode & 07777;
    printf("  MODE | %s | actual=%04o expected=%04o\n",
           msg, actual, expected);
    ASSERT_TRUE(actual == expected, "mode matches expected bits");
}

static void create_file_at(int dfd, const char *name, mode_t mode)
{
    int fd = openat(dfd, name, O_CREAT | O_RDWR | O_TRUNC, 0600);
    ASSERT_TRUE(fd >= 0, "openat creates regular file");
    ASSERT_OK(fchmod(fd, mode), "set initial file mode");
    ASSERT_OK(close(fd), "close created file");
}

static void cleanup(void)
{
    if (dirfd >= 0) {
        close(dirfd);
        dirfd = -1;
    }

    char path[PATH_MAX];
    const char *names[] = {"target", "absolute", "empty-path", NULL};
    for (int i = 0; names[i] != NULL; i++) {
        join_path(path, sizeof(path), base, names[i]);
        unlink(path);
    }
    rmdir(base);
}

static void setup(void)
{
    int ret = snprintf(base, sizeof(base), "/tmp/test-fchmodat-%ld",
                       (long)getpid());
    ASSERT_TRUE(ret > 0 && (size_t)ret < sizeof(base), "build test directory");

    cleanup();

    ASSERT_OK(mkdir(base, 0700), "mkdir test directory");
    dirfd = open(base, O_RDONLY | O_DIRECTORY);
    ASSERT_TRUE(dirfd >= 0, "open test directory");
}

static void test_fchmodat_relative_and_absolute_paths(void)
{
    create_file_at(dirfd, "target", 0600);
    ASSERT_OK(do_fchmodat(dirfd, "target", 04755),
              "fchmodat changes relative path mode");
    assert_mode_at(dirfd, "target", 04755,
                   "stat target after relative fchmodat");

    char absolute[PATH_MAX];
    join_path(absolute, sizeof(absolute), base, "absolute");
    create_file_at(dirfd, "absolute", 0600);
    ASSERT_OK(do_fchmodat(-1, absolute, 0644),
              "fchmodat absolute path ignores dirfd");
    assert_mode_at(dirfd, "absolute", 0644,
                   "stat absolute target after fchmodat");
}

static void test_fchmodat_empty_path(void)
{
    int fd = openat(dirfd, "empty-path", O_CREAT | O_RDWR | O_TRUNC, 0600);
    ASSERT_TRUE(fd >= 0, "open empty-path target");

    ASSERT_ERR(do_fchmodat(fd, "", 0644), ENOENT,
               "fchmodat empty path without AT_EMPTY_PATH returns ENOENT");
    ASSERT_OK(do_fchmodat2(fd, "", 0644, AT_EMPTY_PATH),
              "fchmodat2 empty path changes fd with AT_EMPTY_PATH");
    assert_mode_at(dirfd, "empty-path", 0644,
                   "stat fd target after AT_EMPTY_PATH fchmodat");

    ASSERT_OK(close(fd), "close empty-path fd");
}

static void test_fchmodat_error_returns(void)
{
    ASSERT_ERR(do_fchmodat2(dirfd, "target", 0600, 0x80000000U), EINVAL,
               "fchmodat2 invalid flags returns EINVAL");
    ASSERT_ERR(do_fchmodat(-1, "target", 0600), EBADF,
               "fchmodat relative path with bad dirfd returns EBADF");
}

int main(void)
{
    TEST_START("fchmodat Linux return semantics");

    setup();
    test_fchmodat_relative_and_absolute_paths();
    test_fchmodat_empty_path();
    test_fchmodat_error_returns();
    cleanup();

    TEST_DONE();
}
