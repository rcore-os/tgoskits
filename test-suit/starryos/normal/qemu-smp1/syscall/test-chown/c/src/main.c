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

static void assert_owner_mode(const char *path, uid_t uid, gid_t gid,
                              mode_t mode, const char *msg)
{
    struct stat st;
    ASSERT_OK(stat(path, &st), msg);
    printf("  STAT | %s | uid=%u gid=%u mode=%04o\n",
           msg, (unsigned)st.st_uid, (unsigned)st.st_gid,
           (unsigned)(st.st_mode & 07777));
    ASSERT_TRUE(st.st_uid == uid, "uid matches expected value");
    ASSERT_TRUE(st.st_gid == gid, "gid matches expected value");
    ASSERT_TRUE((st.st_mode & 07777) == mode, "mode matches expected bits");
}

static void cleanup(void)
{
    if (dirfd >= 0) {
        close(dirfd);
        dirfd = -1;
    }

    const char *names[] = {"target", "link-target", "link", "not-dir", NULL};
    for (int i = 0; names[i] != NULL; i++) {
        unlink(path_for(names[i]));
        rmdir(path_for(names[i]));
    }
    rmdir(base);
}

static void setup(void)
{
    int ret = snprintf(base, sizeof(base), "/tmp/test-chown-%ld",
                       (long)getpid());
    ASSERT_TRUE(ret > 0 && (size_t)ret < sizeof(base), "build test directory");

    cleanup();

    ASSERT_OK(mkdir(base, 0700), "mkdir test directory");
    dirfd = open(base, O_RDONLY | O_DIRECTORY);
    ASSERT_TRUE(dirfd >= 0, "open test directory");
}

static void test_chown_changes_path_owner(void)
{
    const char *target = path_for("target");
    create_file_at(dirfd, "target", 06755);

    ASSERT_OK(chown(target, 1234, 2345), "chown changes uid and gid");
    assert_owner_mode(target, 1234, 2345, 0755,
                      "stat target after chown clears setuid");

    ASSERT_OK(chown(target, (uid_t)-1, 3456), "chown -1 preserves uid");
    assert_owner_mode(target, 1234, 3456, 0755,
                      "stat target after chown preserving uid");
}

static void test_chown_follows_symlink(void)
{
    const char *target = path_for("link-target");
    const char *link = path_for("link");
    create_file_at(dirfd, "link-target", 0600);
    ASSERT_OK(symlinkat("link-target", dirfd, "link"), "create symlink");

    ASSERT_OK(chown(link, 2222, 3333), "chown follows symlink target");
    assert_owner_mode(target, 2222, 3333, 0600,
                      "stat symlink target after chown link");
}

static void test_chown_error_returns(void)
{
    char not_dir[PATH_MAX];
    join_path(not_dir, sizeof(not_dir), base, "not-dir");
    create_file_at(dirfd, "not-dir", 0600);

    ASSERT_ERR(chown(path_for("missing"), 1, 1), ENOENT,
               "chown missing path returns ENOENT");
    ASSERT_ERR(chown("", 1, 1), ENOENT, "chown empty path returns ENOENT");

    char child[PATH_MAX];
    join_path(child, sizeof(child), not_dir, "child");
    ASSERT_ERR(chown(child, 1, 1), ENOTDIR,
               "chown through non-directory component returns ENOTDIR");
}

int main(void)
{
    TEST_START("chown Linux return semantics");

    setup();
    test_chown_changes_path_owner();
    test_chown_follows_symlink();
    test_chown_error_returns();
    cleanup();

    TEST_DONE();
}
