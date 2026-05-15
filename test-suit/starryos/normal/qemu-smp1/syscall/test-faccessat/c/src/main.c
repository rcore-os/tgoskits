#define _GNU_SOURCE

#include "test_framework.h"

#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <unistd.h>

#ifndef SYS_faccessat
#define SYS_faccessat 48
#endif

#ifndef SYS_faccessat2
#define SYS_faccessat2 439
#endif

static char base[PATH_MAX];
static int dirfd = -1;

static int do_faccessat(int dfd, const char *path, int mode)
{
    return (int)syscall(SYS_faccessat, dfd, path, mode);
}

static int do_faccessat2(int dfd, const char *path, int mode, int flags)
{
    return (int)syscall(SYS_faccessat2, dfd, path, mode, flags);
}

static void join_path(char *out, size_t out_len, const char *dir,
                      const char *name)
{
    int ret = snprintf(out, out_len, "%s/%s", dir, name);
    ASSERT_TRUE(ret > 0 && (size_t)ret < out_len, "build absolute path");
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
    const char *names[] = {"target", "absolute", "no-exec", NULL};
    for (int i = 0; names[i] != NULL; i++) {
        join_path(path, sizeof(path), base, names[i]);
        unlink(path);
    }
    rmdir(base);
}

static void setup(void)
{
    int ret = snprintf(base, sizeof(base), "/tmp/test-faccessat-%ld",
                       (long)getpid());
    ASSERT_TRUE(ret > 0 && (size_t)ret < sizeof(base), "build test directory");

    cleanup();

    ASSERT_OK(mkdir(base, 0700), "mkdir test directory");
    dirfd = open(base, O_RDONLY | O_DIRECTORY);
    ASSERT_TRUE(dirfd >= 0, "open test directory");
}

static void test_faccessat_relative_and_absolute_paths(void)
{
    create_file_at(dirfd, "target", 0640);
    ASSERT_OK(do_faccessat(dirfd, "target", F_OK),
              "faccessat F_OK succeeds for relative path");
    ASSERT_OK(do_faccessat(dirfd, "target", R_OK | W_OK),
              "root faccessat R_OK|W_OK succeeds");

    char absolute[PATH_MAX];
    join_path(absolute, sizeof(absolute), base, "absolute");
    create_file_at(dirfd, "absolute", 0600);
    ASSERT_OK(do_faccessat(-1, absolute, F_OK),
              "faccessat absolute path ignores dirfd");
}

static void test_faccessat_execute_and_flags(void)
{
    create_file_at(dirfd, "no-exec", 0600);
    ASSERT_ERR(do_faccessat(dirfd, "no-exec", X_OK), EACCES,
               "root faccessat X_OK requires at least one execute bit");
    ASSERT_OK(fchmodat(dirfd, "no-exec", 0700, 0),
              "set execute bit for faccessat");
    ASSERT_OK(do_faccessat2(dirfd, "no-exec", X_OK, AT_EACCESS),
              "faccessat2 accepts AT_EACCESS and executable mode");
}

static void test_faccessat_error_returns(void)
{
    ASSERT_ERR(do_faccessat(dirfd, "missing", F_OK), ENOENT,
               "faccessat missing path returns ENOENT");
    ASSERT_ERR(do_faccessat(dirfd, "target", R_OK | 0x80), EINVAL,
               "faccessat invalid mode returns EINVAL");
    ASSERT_ERR(do_faccessat2(dirfd, "target", F_OK, 0x80000000U), EINVAL,
               "faccessat2 invalid flags returns EINVAL");
    ASSERT_ERR(do_faccessat(-1, "target", F_OK), EBADF,
               "faccessat relative path with bad dirfd returns EBADF");
}

int main(void)
{
    TEST_START("faccessat Linux return semantics");

    setup();
    test_faccessat_relative_and_absolute_paths();
    test_faccessat_execute_and_flags();
    test_faccessat_error_returns();
    cleanup();

    TEST_DONE();
}
