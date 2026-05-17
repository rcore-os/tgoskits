#define _GNU_SOURCE

#include "test_framework.h"

#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

static char base[PATH_MAX];
static char scratch[PATH_MAX];
static int dirfd = -1;

static void join_path(char *out, size_t out_len, const char *dir,
                      const char *name)
{
    int ret = snprintf(out, out_len, "%s/%s", dir, name);
    ASSERT_TRUE(ret > 0 && (size_t)ret < out_len, "build absolute path");
}

static const char *path_for(const char *name)
{
    join_path(scratch, sizeof(scratch), base, name);
    return scratch;
}

static void create_file_at(int dfd, const char *name, mode_t mode)
{
    int fd = openat(dfd, name, O_CREAT | O_RDWR | O_TRUNC, 0600);
    ASSERT_TRUE(fd >= 0, "openat creates regular file");
    ASSERT_OK(fchmod(fd, mode), "set initial file mode");
    ASSERT_OK(close(fd), "close created file");
}

static void assert_mode(const char *path, mode_t expected, const char *msg)
{
    struct stat st;
    ASSERT_OK(stat(path, &st), msg);
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

    const char *names[] = {
        "target",
        "link-target",
        "link",
        "not-dir",
        NULL,
    };

    for (int i = 0; names[i] != NULL; i++) {
        unlink(path_for(names[i]));
        rmdir(path_for(names[i]));
    }
    rmdir(base);
}

static void setup(void)
{
    int ret = snprintf(base, sizeof(base), "/tmp/test-chmod-%ld",
                       (long)getpid());
    ASSERT_TRUE(ret > 0 && (size_t)ret < sizeof(base), "build test directory");

    cleanup();

    ASSERT_OK(mkdir(base, 0700), "mkdir test directory");
    dirfd = open(base, O_RDONLY | O_DIRECTORY);
    ASSERT_TRUE(dirfd >= 0, "open test directory");
}

static void test_chmod_changes_path_mode(void)
{
    const char *target = path_for("target");
    create_file_at(dirfd, "target", 0600);

    ASSERT_OK(chmod(target, 04755), "chmod sets rwx and setuid bits");
    assert_mode(target, 04755, "stat target after chmod 04755");

    ASSERT_OK(chmod(target, 0100644), "chmod ignores file type bits in mode");
    assert_mode(target, 0644, "stat target after chmod with file type bits");

}

static void test_chmod_follows_symlink(void)
{
    const char *target = path_for("link-target");
    const char *link = path_for("link");
    create_file_at(dirfd, "link-target", 0600);
    ASSERT_OK(symlinkat("link-target", dirfd, "link"), "create symlink");

    ASSERT_OK(chmod(link, 0644), "chmod follows symlink target");
    assert_mode(target, 0644, "stat symlink target after chmod link");
}

static void test_chmod_error_returns(void)
{
    char not_dir[PATH_MAX];
    join_path(not_dir, sizeof(not_dir), base, "not-dir");
    create_file_at(dirfd, "not-dir", 0600);

    ASSERT_ERR(chmod(path_for("missing"), 0600), ENOENT,
               "chmod missing path returns ENOENT");
    ASSERT_ERR(chmod("", 0600), ENOENT, "chmod empty path returns ENOENT");

    char child[PATH_MAX];
    join_path(child, sizeof(child), not_dir, "child");
    ASSERT_ERR(chmod(child, 0600), ENOTDIR,
               "chmod through non-directory component returns ENOTDIR");
}

int main(void)
{
    TEST_START("chmod Linux return semantics");

    setup();
    test_chmod_changes_path_mode();
    test_chmod_follows_symlink();
    test_chmod_error_returns();
    cleanup();

    TEST_DONE();
}
