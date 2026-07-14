#define _GNU_SOURCE

#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/xattr.h>
#include <unistd.h>

#ifndef O_PATH
#define O_PATH 010000000
#endif

static char base[PATH_MAX];
static char scratch[PATH_MAX];
static int dirfd = -1;

static void join_path(char *out, size_t out_len, const char *dir,
                      const char *name)
{
    int ret = snprintf(out, out_len, "%s/%s", dir, name);
    CHECK(ret > 0 && (size_t)ret < out_len, "build absolute path");
}

static const char *path_for(const char *name)
{
    join_path(scratch, sizeof(scratch), base, name);
    return scratch;
}

static void cleanup(void)
{
    if (dirfd >= 0) {
        close(dirfd);
        dirfd = -1;
    }

    const char *names[] = {
        "target",
        "target-hardlink",
        "target-symlink",
        "copy-dst",
        NULL,
    };

    for (int i = 0; names[i] != NULL; i++) {
        unlink(path_for(names[i]));
    }
    rmdir(base);
}

static void setup(void)
{
    int ret = snprintf(base, sizeof(base), "/tmp/test-xattr-%ld",
                       (long)getpid());
    CHECK(ret > 0 && (size_t)ret < sizeof(base), "build test directory");

    cleanup();

    CHECK_RET(mkdir(base, 0700), 0, "mkdir test directory");
    dirfd = open(base, O_RDONLY | O_DIRECTORY);
    CHECK(dirfd >= 0, "open test directory");

    int fd = openat(dirfd, "target", O_CREAT | O_RDWR | O_TRUNC, 0600);
    CHECK(fd >= 0, "create target file");
    CHECK_RET(write(fd, "xattr-test\n", 11), 11, "write target file");
    CHECK_RET(close(fd), 0, "close target file");
}

static void expect_value(const char *path, const char *name,
                         const char *expected, const char *msg)
{
    char buf[64];
    memset(buf, 0, sizeof(buf));
    ssize_t size = getxattr(path, name, buf, sizeof(buf));
    CHECK(size == (ssize_t)strlen(expected), msg);
    CHECK(memcmp(buf, expected, strlen(expected)) == 0, "xattr value matches");
}

static void expect_fd_value(int fd, const char *name, const char *expected,
                            const char *msg)
{
    char buf[64];
    memset(buf, 0, sizeof(buf));
    ssize_t size = fgetxattr(fd, name, buf, sizeof(buf));
    CHECK(size == (ssize_t)strlen(expected), msg);
    CHECK(memcmp(buf, expected, strlen(expected)) == 0, "fd xattr value matches");
}

static void expect_single_name(const char *path, const char *expected_name,
                               const char *msg)
{
    char list[128];
    memset(list, 0, sizeof(list));
    size_t expected_len = strlen(expected_name) + 1;

    CHECK_RET(listxattr(path, NULL, 0), (long)expected_len,
              "listxattr size query");
    ssize_t size = listxattr(path, list, sizeof(list));
    CHECK(size == (ssize_t)expected_len, msg);
    CHECK(memcmp(list, expected_name, expected_len - 1) == 0,
          "listxattr name matches");
    CHECK(list[expected_len - 1] == '\0', "listxattr name is nul terminated");
}

static void test_path_xattr(void)
{
    const char *target = path_for("target");

    CHECK_RET(listxattr(target, NULL, 0), 0, "empty listxattr size");
    CHECK_ERR(getxattr(target, "user.test", NULL, 0), ENODATA,
              "missing getxattr returns ENODATA");

    CHECK_RET(setxattr(target, "user.test", "alpha", 5, 0), 0,
              "setxattr creates user attribute");
    CHECK_RET(getxattr(target, "user.test", NULL, 0), 5,
              "getxattr size query");
    expect_value(target, "user.test", "alpha", "getxattr reads value");
    expect_single_name(target, "user.test", "listxattr returns user.test");

    char small[4];
    CHECK_ERR(getxattr(target, "user.test", small, sizeof(small)), ERANGE,
              "getxattr rejects short buffer");
    CHECK_ERR(listxattr(target, small, sizeof(small)), ERANGE,
              "listxattr rejects short buffer");

    CHECK_ERR(setxattr(target, "user.test", "again", 5, XATTR_CREATE),
              EEXIST, "XATTR_CREATE rejects existing attribute");
    CHECK_RET(setxattr(target, "user.test", "beta", 4, XATTR_REPLACE), 0,
              "XATTR_REPLACE updates existing attribute");
    expect_value(target, "user.test", "beta", "replace value is visible");
    CHECK_ERR(setxattr(target, "user.missing", "x", 1, XATTR_REPLACE),
              ENODATA, "XATTR_REPLACE rejects missing attribute");
    CHECK_ERR(setxattr(target, "user.badflags", "x", 1,
                       XATTR_CREATE | XATTR_REPLACE),
              EINVAL, "mutually exclusive flags are rejected");

    CHECK_ERR(setxattr(target, "security.test", "x", 1, 0), EOPNOTSUPP,
              "unsupported namespace returns EOPNOTSUPP");
    CHECK_ERR(getxattr(target, "security.test", NULL, 0), EOPNOTSUPP,
              "getxattr validates unsupported namespace");

    CHECK_RET(removexattr(target, "user.test"), 0, "removexattr removes attr");
    CHECK_ERR(getxattr(target, "user.test", NULL, 0), ENODATA,
              "removed attr is not visible");
    CHECK_RET(listxattr(target, NULL, 0), 0, "listxattr empty after remove");
    CHECK_ERR(removexattr(target, "user.test"), ENODATA,
              "removing missing attr returns ENODATA");
}

static void test_fd_xattr(void)
{
    const char *target = path_for("target");
    int fd = open(target, O_RDWR);
    CHECK(fd >= 0, "open target for f*xattr");

    CHECK_RET(flistxattr(fd, NULL, 0), 0, "empty flistxattr size");
    CHECK_RET(fsetxattr(fd, "user.fd", "fd-value", 8, 0), 0,
              "fsetxattr creates fd attribute");
    CHECK_RET(fgetxattr(fd, "user.fd", NULL, 0), 8,
              "fgetxattr size query");
    expect_fd_value(fd, "user.fd", "fd-value", "fgetxattr reads value");

    char list[128];
    memset(list, 0, sizeof(list));
    CHECK_RET(flistxattr(fd, list, sizeof(list)), 8,
              "flistxattr returns fd attr name");
    CHECK(memcmp(list, "user.fd", 7) == 0 && list[7] == '\0',
          "flistxattr name matches");

    CHECK_RET(fremovexattr(fd, "user.fd"), 0, "fremovexattr removes attr");
    CHECK_ERR(fgetxattr(fd, "user.fd", NULL, 0), ENODATA,
              "fremoved attr is not visible");
    CHECK_RET(close(fd), 0, "close f*xattr fd");
}

static void test_o_path_fd_rejected(void)
{
    const char *target = path_for("target");
    int fd = open(target, O_PATH);
    CHECK(fd >= 0, "open O_PATH fd");
    if (fd < 0) {
        return;
    }

    CHECK_ERR(flistxattr(fd, NULL, 0), EBADF, "flistxattr rejects O_PATH fd");
    CHECK_ERR(fgetxattr(fd, "user.path", NULL, 0), EBADF,
              "fgetxattr rejects O_PATH fd");
    CHECK_ERR(fsetxattr(fd, "user.path", "x", 1, 0), EBADF,
              "fsetxattr rejects O_PATH fd");
    CHECK_ERR(fremovexattr(fd, "user.path"), EBADF,
              "fremovexattr rejects O_PATH fd");
    CHECK_RET(close(fd), 0, "close O_PATH fd");
}

static void test_link_and_symlink_xattr(void)
{
    char target[PATH_MAX];
    char hardlink[PATH_MAX];
    char symlink_path[PATH_MAX];
    join_path(target, sizeof(target), base, "target");
    join_path(hardlink, sizeof(hardlink), base, "target-hardlink");
    join_path(symlink_path, sizeof(symlink_path), base, "target-symlink");

    CHECK_RET(setxattr(target, "user.shared", "shared", 6, 0), 0,
              "set attr on target before link checks");
    CHECK_RET(link(target, hardlink), 0, "create hard link");
    expect_value(hardlink, "user.shared", "shared",
                 "hard link sees inode xattr");

    CHECK_RET(symlink("target", symlink_path), 0, "create relative symlink");
    expect_value(symlink_path, "user.shared", "shared",
                 "getxattr follows symlink target");

    CHECK_RET(setxattr(symlink_path, "user.follow", "follow", 6, 0), 0,
              "setxattr follows symlink target");
    expect_value(target, "user.follow", "follow",
                 "target sees attr set through symlink");

    CHECK_RET(lsetxattr(symlink_path, "user.link", "link", 4, 0), 0,
              "lsetxattr stores attr on symlink itself");
    char buf[32];
    memset(buf, 0, sizeof(buf));
    CHECK_RET(lgetxattr(symlink_path, "user.link", buf, sizeof(buf)), 4,
              "lgetxattr reads symlink attr");
    CHECK(memcmp(buf, "link", 4) == 0, "symlink xattr value matches");
    CHECK_ERR(getxattr(target, "user.link", NULL, 0), ENODATA,
              "symlink attr is separate from target attr");
}

static void test_copy_loop_compatibility(void)
{
    char src[PATH_MAX];
    char dst[PATH_MAX];
    join_path(src, sizeof(src), base, "target");
    join_path(dst, sizeof(dst), base, "copy-dst");

    int fd = open(dst, O_CREAT | O_RDWR | O_TRUNC, 0600);
    CHECK(fd >= 0, "create copy destination");
    CHECK_RET(close(fd), 0, "close copy destination");

    CHECK_RET(setxattr(src, "user.copy", "copy-value", 10, 0),
              0, "set source copy attr");

    char names[256];
    ssize_t names_len = listxattr(src, names, sizeof(names));
    CHECK(names_len > 0, "copy loop source has names");

    ssize_t off = 0;
    while (off < names_len) {
        const char *name = names + off;
        size_t name_len = strlen(name);
        if (name_len == 0) {
            break;
        }

        char value[256];
        ssize_t value_len = getxattr(src, name, value, sizeof(value));
        CHECK(value_len >= 0, "copy loop getxattr succeeds");
        if (value_len >= 0) {
            CHECK_RET(setxattr(dst, name, value, (size_t)value_len, 0), 0,
                      "copy loop setxattr succeeds");
        }

        off += (ssize_t)name_len + 1;
    }

    expect_value(dst, "user.copy", "copy-value",
                 "copied xattr is visible on destination");
}

int main(void)
{
    TEST_START("xattr syscalls");

    setup();
    test_path_xattr();
    test_fd_xattr();
    test_o_path_fd_rejected();
    test_link_and_symlink_xattr();
    test_copy_loop_compatibility();
    cleanup();

    TEST_DONE();
}
