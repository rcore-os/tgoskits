#define _GNU_SOURCE

#include "test_framework.h"

#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

#ifndef AT_EMPTY_PATH
#define AT_EMPTY_PATH 0x1000
#endif

#define UID_A ((uid_t)1001)
#define GID_A ((gid_t)1002)
#define UID_B ((uid_t)1003)
#define GID_B ((gid_t)1004)
#define UID_C ((uid_t)1005)
#define GID_C ((gid_t)1006)
#define UID_D ((uid_t)1007)
#define GID_D ((gid_t)1008)
#define UID_E ((uid_t)1009)
#define GID_E ((gid_t)1010)

static char base[PATH_MAX];
static char absolute_path[PATH_MAX];
static int dirfd = -1;

static void join_path(char *out, size_t out_len, const char *dir,
                      const char *name)
{
    int ret = snprintf(out, out_len, "%s/%s", dir, name);
    ASSERT_TRUE(ret > 0 && (size_t)ret < out_len, "build absolute path");
}

static int create_file_at(int dfd, const char *name)
{
    int fd = openat(dfd, name, O_CREAT | O_RDWR | O_TRUNC, 0600);
    ASSERT_TRUE(fd >= 0, "openat creates regular file");
    ASSERT_OK(close(fd), "close created regular file");
    return 0;
}

static void assert_owner_at(int dfd, const char *path, int flags,
                            uid_t uid, gid_t gid, const char *msg)
{
    struct stat st;
    ASSERT_OK(fstatat(dfd, path, &st, flags), msg);
    printf("  OWNER | %s | uid=%ld gid=%ld expected=%ld:%ld\n",
           msg, (long)st.st_uid, (long)st.st_gid, (long)uid, (long)gid);
    ASSERT_TRUE((uid_t)st.st_uid == uid, "uid matches expected owner");
    ASSERT_TRUE((gid_t)st.st_gid == gid, "gid matches expected group");
}

static void cleanup(void)
{
    if (dirfd >= 0) {
        close(dirfd);
        dirfd = -1;
    }

    char path[PATH_MAX];
    const char *names[] = {
        "target",
        "cwd-target",
        "absolute-target",
        "empty-path-target",
        "link-target",
        "link",
        "nofollow-target",
        "nofollow-link",
        "not-dir",
        NULL,
    };

    for (int i = 0; names[i] != NULL; i++) {
        join_path(path, sizeof(path), base, names[i]);
        unlink(path);
    }
    rmdir(base);
}

static void setup(void)
{
    int ret = snprintf(base, sizeof(base), "/tmp/test-fchownat-%ld",
                       (long)getpid());
    ASSERT_TRUE(ret > 0 && (size_t)ret < sizeof(base), "build test directory");

    cleanup();

    ASSERT_OK(mkdir(base, 0700), "mkdir test directory");
    dirfd = open(base, O_RDONLY | O_DIRECTORY);
    ASSERT_TRUE(dirfd >= 0, "open test directory");
}

static void test_relative_path_uses_dirfd(void)
{
    create_file_at(dirfd, "target");

    ASSERT_OK(fchownat(dirfd, "target", UID_A, GID_A, 0),
              "relative path is resolved against dirfd");
    assert_owner_at(dirfd, "target", 0, UID_A, GID_A,
                    "stat relative target after fchownat");
}

static void test_minus_one_leaves_id_unchanged(void)
{
    ASSERT_OK(fchownat(dirfd, "target", (uid_t)-1, GID_B, 0),
              "owner -1 leaves uid unchanged while group changes");
    assert_owner_at(dirfd, "target", 0, UID_A, GID_B,
                    "stat target after owner -1");

    ASSERT_OK(fchownat(dirfd, "target", UID_B, (gid_t)-1, 0),
              "group -1 leaves gid unchanged while owner changes");
    assert_owner_at(dirfd, "target", 0, UID_B, GID_B,
                    "stat target after group -1");
}

static void test_at_fdcwd_relative_path(void)
{
    char old_cwd[PATH_MAX];
    ASSERT_TRUE(getcwd(old_cwd, sizeof(old_cwd)) != NULL, "save cwd");
    ASSERT_OK(chdir(base), "chdir test directory");

    int fd = open("cwd-target", O_CREAT | O_RDWR | O_TRUNC, 0600);
    ASSERT_TRUE(fd >= 0, "create cwd-relative file");
    ASSERT_OK(close(fd), "close cwd-relative file");
    ASSERT_OK(fchownat(AT_FDCWD, "cwd-target", UID_C, GID_C, 0),
              "AT_FDCWD resolves relative path against cwd");
    assert_owner_at(AT_FDCWD, "cwd-target", 0, UID_C, GID_C,
                    "stat AT_FDCWD target");

    ASSERT_OK(chdir(old_cwd), "restore cwd");
}

static void test_absolute_path_ignores_dirfd(void)
{
    create_file_at(dirfd, "absolute-target");
    join_path(absolute_path, sizeof(absolute_path), base, "absolute-target");

    ASSERT_OK(fchownat(-1, absolute_path, UID_D, GID_D, 0),
              "absolute path ignores invalid dirfd");
    assert_owner_at(AT_FDCWD, absolute_path, 0, UID_D, GID_D,
                    "stat absolute target");
}

static void test_at_empty_path_targets_dirfd_file(void)
{
    create_file_at(dirfd, "empty-path-target");

    int fd = openat(dirfd, "empty-path-target", O_RDONLY);
    ASSERT_TRUE(fd >= 0, "open file for AT_EMPTY_PATH");
    ASSERT_OK(fchownat(fd, "", UID_E, GID_E, AT_EMPTY_PATH),
              "AT_EMPTY_PATH operates on file referred to by dirfd");
    ASSERT_OK(close(fd), "close AT_EMPTY_PATH fd");
    assert_owner_at(dirfd, "empty-path-target", 0, UID_E, GID_E,
                    "stat AT_EMPTY_PATH target");
}

static void test_symlink_follow_and_nofollow(void)
{
    create_file_at(dirfd, "link-target");
    ASSERT_OK(symlinkat("link-target", dirfd, "link"), "create symlink");
    ASSERT_OK(fchownat(dirfd, "link", UID_A, GID_A, 0),
              "default fchownat follows symlink");
    assert_owner_at(dirfd, "link", 0, UID_A, GID_A,
                    "stat followed symlink target");

    create_file_at(dirfd, "nofollow-target");
    ASSERT_OK(symlinkat("nofollow-target", dirfd, "nofollow-link"),
              "create nofollow symlink");
    ASSERT_OK(fchownat(dirfd, "nofollow-link", UID_B, GID_B,
                       AT_SYMLINK_NOFOLLOW),
              "AT_SYMLINK_NOFOLLOW changes symlink itself");
    assert_owner_at(dirfd, "nofollow-link", AT_SYMLINK_NOFOLLOW, UID_B, GID_B,
                    "lstat nofollow symlink");
}

static void test_error_returns(void)
{
    ASSERT_ERR(fchownat(-1, "target", UID_A, GID_A, 0), EBADF,
               "relative path with invalid dirfd returns EBADF");

    ASSERT_ERR(fchownat(dirfd, "target", UID_A, GID_A, AT_REMOVEDIR), EINVAL,
               "invalid flags return EINVAL");

    int fd = openat(dirfd, "not-dir", O_CREAT | O_RDWR | O_TRUNC, 0600);
    ASSERT_TRUE(fd >= 0, "create non-directory dirfd candidate");
    ASSERT_ERR(fchownat(fd, "child", UID_A, GID_A, 0), ENOTDIR,
               "relative path with file dirfd returns ENOTDIR");
    ASSERT_OK(close(fd), "close non-directory fd");

    ASSERT_ERR(fchownat(dirfd, "missing", UID_A, GID_A, 0), ENOENT,
               "missing path returns ENOENT");
}

int main(void)
{
    TEST_START("fchownat Linux return semantics");

    setup();
    test_relative_path_uses_dirfd();
    test_minus_one_leaves_id_unchanged();
    test_at_fdcwd_relative_path();
    test_absolute_path_ignores_dirfd();
    test_at_empty_path_targets_dirfd_file();
    test_symlink_follow_and_nofollow();
    test_error_returns();
    cleanup();

    TEST_DONE();
}
