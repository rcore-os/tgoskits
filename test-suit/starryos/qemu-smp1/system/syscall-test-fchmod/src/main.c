#define _GNU_SOURCE

#include "test_framework.h"

#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

static char base[PATH_MAX];
static int dirfd = -1;

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

static void cleanup(void)
{
    if (dirfd >= 0) {
        close(dirfd);
        dirfd = -1;
    }

    char path[PATH_MAX];
    const char *names[] = {"target", "dir-target", NULL};
    for (int i = 0; names[i] != NULL; i++) {
        join_path(path, sizeof(path), base, names[i]);
        unlink(path);
        rmdir(path);
    }
    rmdir(base);
}

static void setup(void)
{
    int ret = snprintf(base, sizeof(base), "/tmp/test-fchmod-%ld",
                       (long)getpid());
    ASSERT_TRUE(ret > 0 && (size_t)ret < sizeof(base), "build test directory");

    cleanup();

    ASSERT_OK(mkdir(base, 0700), "mkdir test directory");
    dirfd = open(base, O_RDONLY | O_DIRECTORY);
    ASSERT_TRUE(dirfd >= 0, "open test directory");
}

static void test_fchmod_changes_open_file_mode(void)
{
    int fd = openat(dirfd, "target", O_CREAT | O_RDWR | O_TRUNC, 0600);
    ASSERT_TRUE(fd >= 0, "openat creates regular file");

    ASSERT_OK(fchmod(fd, 04755), "fchmod sets rwx and setuid bits");
    assert_mode_at(dirfd, "target", 04755, "stat target after fchmod 04755");

    ASSERT_OK(fchmod(fd, 0100644), "fchmod ignores file type bits in mode");
    assert_mode_at(dirfd, "target", 0644,
                   "stat target after fchmod with file type bits");

    ASSERT_OK(close(fd), "close regular file");
}

static void test_fchmod_changes_directory_fd_mode(void)
{
    ASSERT_OK(mkdirat(dirfd, "dir-target", 0700), "mkdir target directory");
    int fd = openat(dirfd, "dir-target", O_RDONLY | O_DIRECTORY);
    ASSERT_TRUE(fd >= 0, "open directory fd");

    ASSERT_OK(fchmod(fd, 01755), "fchmod changes directory fd mode");
    assert_mode_at(dirfd, "dir-target", 01755,
                   "stat directory after fchmod");

    ASSERT_OK(close(fd), "close directory fd");
}

static void test_fchmod_error_returns(void)
{
    ASSERT_ERR(fchmod(-1, 0600), EBADF, "fchmod invalid fd returns EBADF");
}

int main(void)
{
    TEST_START("fchmod Linux return semantics");

    setup();
    test_fchmod_changes_open_file_mode();
    test_fchmod_changes_directory_fd_mode();
    test_fchmod_error_returns();
    cleanup();

    TEST_DONE();
}
