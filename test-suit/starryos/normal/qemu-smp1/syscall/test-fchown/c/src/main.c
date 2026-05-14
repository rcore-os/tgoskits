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

static void assert_owner_mode_at(int dfd, const char *path, uid_t uid,
                                 gid_t gid, mode_t mode, const char *msg)
{
    struct stat st;
    ASSERT_OK(fstatat(dfd, path, &st, 0), msg);
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

    char path[PATH_MAX];
    join_path(path, sizeof(path), base, "target");
    unlink(path);
    rmdir(base);
}

static void setup(void)
{
    int ret = snprintf(base, sizeof(base), "/tmp/test-fchown-%ld",
                       (long)getpid());
    ASSERT_TRUE(ret > 0 && (size_t)ret < sizeof(base), "build test directory");

    cleanup();

    ASSERT_OK(mkdir(base, 0700), "mkdir test directory");
    dirfd = open(base, O_RDONLY | O_DIRECTORY);
    ASSERT_TRUE(dirfd >= 0, "open test directory");
}

static void test_fchown_changes_open_file_owner(void)
{
    int fd = openat(dirfd, "target", O_CREAT | O_RDWR | O_TRUNC, 0600);
    ASSERT_TRUE(fd >= 0, "openat creates regular file");
    ASSERT_OK(fchmod(fd, 06755), "set initial file mode");

    ASSERT_OK(fchown(fd, 1234, 2345), "fchown changes uid and gid");
    assert_owner_mode_at(dirfd, "target", 1234, 2345, 0755,
                         "stat target after fchown clears setuid");

    ASSERT_OK(fchown(fd, (uid_t)-1, 3456), "fchown -1 preserves uid");
    assert_owner_mode_at(dirfd, "target", 1234, 3456, 0755,
                         "stat target after fchown preserving uid");

    ASSERT_OK(close(fd), "close regular file");
}

static void test_fchown_error_returns(void)
{
    ASSERT_ERR(fchown(-1, 1, 1), EBADF, "fchown invalid fd returns EBADF");
}

int main(void)
{
    TEST_START("fchown Linux return semantics");

    setup();
    test_fchown_changes_open_file_owner();
    test_fchown_error_returns();
    cleanup();

    TEST_DONE();
}
