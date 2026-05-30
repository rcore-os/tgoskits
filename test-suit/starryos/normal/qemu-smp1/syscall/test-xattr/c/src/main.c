#define _GNU_SOURCE
#include "test_framework.h"
#include <errno.h>
#include <fcntl.h>
#include <string.h>
#include <sys/types.h>
#include <sys/xattr.h>
#include <unistd.h>

#define TMP_FILE "/tmp/test-xattr-file.txt"
#define TMP_LINK "/tmp/test-xattr-link.txt"
#define TMP_DST "/tmp/test-xattr-dst.txt"
#define ROOT_FILE "/root/test-xattr-file.txt"

static int create_file(const char *path, const char *data) {
    unlink(path);
    int fd = open(path, O_CREAT | O_TRUNC | O_RDWR, 0644);
    CHECK(fd >= 0, "create test file");
    if (fd < 0) {
        return -1;
    }
    ssize_t len = (ssize_t)strlen(data);
    CHECK_RET(write(fd, data, (size_t)len), len, "write test file");
    close(fd);
    return 0;
}

static int list_has_name(const char *list, ssize_t len, const char *needle) {
    ssize_t off = 0;
    while (off < len) {
        const char *name = list + off;
        size_t n = strlen(name);
        if (n == 0) {
            return 0;
        }
        if (strcmp(name, needle) == 0) {
            return 1;
        }
        off += (ssize_t)n + 1;
    }
    return 0;
}

static void check_value(const char *path, const char *name, const char *expected) {
    char buf[64];
    ssize_t len = (ssize_t)strlen(expected);
    memset(buf, 0, sizeof(buf));
    CHECK_RET(getxattr(path, name, NULL, 0), len, "getxattr size query");
    CHECK_RET(getxattr(path, name, buf, sizeof(buf)), len, "getxattr value");
    CHECK(memcmp(buf, expected, (size_t)len) == 0, "getxattr value bytes");
}

static void check_fd_value(int fd, const char *name, const char *expected) {
    char buf[64];
    ssize_t len = (ssize_t)strlen(expected);
    memset(buf, 0, sizeof(buf));
    CHECK_RET(fgetxattr(fd, name, NULL, 0), len, "fgetxattr size query");
    CHECK_RET(fgetxattr(fd, name, buf, sizeof(buf)), len, "fgetxattr value");
    CHECK(memcmp(buf, expected, (size_t)len) == 0, "fgetxattr value bytes");
}

static void test_tmpfs_xattrs(void) {
    CHECK(create_file(TMP_FILE, "tmpfs xattr\n") == 0, "prepare tmpfs file");

    CHECK_RET(listxattr(TMP_FILE, NULL, 0), 0, "empty tmpfs xattr list");
    CHECK_ERR(getxattr(TMP_FILE, "user.alpha", NULL, 0), ENODATA,
              "missing tmpfs xattr returns ENODATA");

    CHECK_RET(setxattr(TMP_FILE, "user.alpha", "first", 5, XATTR_CREATE), 0,
              "setxattr creates user.alpha");
    check_value(TMP_FILE, "user.alpha", "first");

    CHECK_ERR(setxattr(TMP_FILE, "user.alpha", "again", 5, XATTR_CREATE), EEXIST,
              "XATTR_CREATE rejects existing attr");
    CHECK_ERR(setxattr(TMP_FILE, "user.missing", "x", 1, XATTR_REPLACE), ENODATA,
              "XATTR_REPLACE rejects missing attr");

    CHECK_RET(setxattr(TMP_FILE, "user.alpha", "second", 6, XATTR_REPLACE), 0,
              "XATTR_REPLACE updates existing attr");
    check_value(TMP_FILE, "user.alpha", "second");

    int fd = open(TMP_FILE, O_RDWR);
    CHECK(fd >= 0, "open tmpfs file for fd xattr");
    if (fd >= 0) {
        CHECK_RET(fsetxattr(fd, "user.beta", "fd", 2, XATTR_CREATE), 0,
                  "fsetxattr creates user.beta");
        check_fd_value(fd, "user.beta", "fd");

        char names[128];
        ssize_t expected = (ssize_t)(strlen("user.alpha") + 1 + strlen("user.beta") + 1);
        memset(names, 0, sizeof(names));
        CHECK_RET(flistxattr(fd, names, sizeof(names)), expected,
                  "flistxattr returns both names");
        CHECK(list_has_name(names, expected, "user.alpha"), "flistxattr contains user.alpha");
        CHECK(list_has_name(names, expected, "user.beta"), "flistxattr contains user.beta");
        close(fd);
    }

    char names[128];
    ssize_t expected = (ssize_t)(strlen("user.alpha") + 1 + strlen("user.beta") + 1);
    memset(names, 0, sizeof(names));
    CHECK_RET(listxattr(TMP_FILE, NULL, 0), expected, "listxattr size query");
    CHECK_RET(listxattr(TMP_FILE, names, sizeof(names)), expected, "listxattr names");
    CHECK(list_has_name(names, expected, "user.alpha"), "listxattr contains user.alpha");
    CHECK(list_has_name(names, expected, "user.beta"), "listxattr contains user.beta");

    char small[1];
    CHECK_ERR(listxattr(TMP_FILE, small, sizeof(small)), ERANGE,
              "listxattr rejects too-small buffer");
    CHECK_ERR(getxattr(TMP_FILE, "user.alpha", small, sizeof(small)), ERANGE,
              "getxattr rejects too-small buffer");

    CHECK_RET(lsetxattr(TMP_FILE, "user.gamma", "link", 4, XATTR_CREATE), 0,
              "lsetxattr works on regular file");
    CHECK_RET(lgetxattr(TMP_FILE, "user.gamma", NULL, 0), 4,
              "lgetxattr reads regular file attr");
    CHECK_RET(lremovexattr(TMP_FILE, "user.gamma"), 0,
              "lremovexattr removes regular file attr");

    unlink(TMP_LINK);
    CHECK_RET(symlink(TMP_FILE, TMP_LINK), 0, "create symlink for l* xattr");
    CHECK_RET(lsetxattr(TMP_LINK, "user.symlink", "ln", 2, XATTR_CREATE), 0,
              "lsetxattr stores attr on symlink itself");
    CHECK_RET(lgetxattr(TMP_LINK, "user.symlink", NULL, 0), 2,
              "lgetxattr sees symlink attr");
    CHECK_ERR(getxattr(TMP_LINK, "user.symlink", NULL, 0), ENODATA,
              "getxattr follows symlink to target");
    CHECK_RET(lremovexattr(TMP_LINK, "user.symlink"), 0,
              "lremovexattr removes symlink attr");
    unlink(TMP_LINK);

    CHECK_ERR(setxattr(TMP_FILE, "user.badflags", "x", 1,
                       XATTR_CREATE | XATTR_REPLACE),
              EINVAL, "setxattr rejects CREATE|REPLACE");
    CHECK_ERR(setxattr(TMP_FILE, "security.bad", "x", 1, 0), EOPNOTSUPP,
              "setxattr rejects unsupported namespace");
    CHECK_ERR(setxattr(TMP_FILE, "", "x", 1, 0), ERANGE,
              "setxattr rejects empty name");

#ifdef O_PATH
    int path_fd = open(TMP_FILE, O_PATH);
    CHECK(path_fd >= 0, "open tmpfs file with O_PATH");
    if (path_fd >= 0) {
        CHECK_ERR(flistxattr(path_fd, NULL, 0), EBADF, "flistxattr rejects O_PATH fd");
        CHECK_ERR(fgetxattr(path_fd, "user.alpha", NULL, 0), EBADF,
                  "fgetxattr rejects O_PATH fd");
        CHECK_ERR(fsetxattr(path_fd, "user.path", "x", 1, 0), EBADF,
                  "fsetxattr rejects O_PATH fd");
        CHECK_ERR(fremovexattr(path_fd, "user.alpha"), EBADF,
                  "fremovexattr rejects O_PATH fd");
        close(path_fd);
    }
#endif

    CHECK_RET(removexattr(TMP_FILE, "user.alpha"), 0, "removexattr removes user.alpha");
    CHECK_ERR(getxattr(TMP_FILE, "user.alpha", NULL, 0), ENODATA,
              "removed xattr is gone");
    CHECK_ERR(removexattr(TMP_FILE, "user.alpha"), ENODATA,
              "removing missing tmpfs xattr returns ENODATA");

    fd = open(TMP_FILE, O_RDWR);
    CHECK(fd >= 0, "open tmpfs file for fremovexattr");
    if (fd >= 0) {
        CHECK_RET(fremovexattr(fd, "user.beta"), 0, "fremovexattr removes user.beta");
        close(fd);
    }
    CHECK_RET(listxattr(TMP_FILE, NULL, 0), 0, "tmpfs xattr list is empty after removal");
}

static void test_copyxattr_pattern(void) {
    CHECK(create_file(TMP_FILE, "copy source\n") == 0, "prepare copy source");
    CHECK(create_file(TMP_DST, "copy destination\n") == 0, "prepare copy destination");

    CHECK_RET(setxattr(TMP_FILE, "user.copy", "payload", 7, XATTR_CREATE), 0,
              "source xattr created");

    char names[128];
    char value[128];
    ssize_t names_len = listxattr(TMP_FILE, names, sizeof(names));
    CHECK(names_len > 0, "copy source has xattr names");

    ssize_t off = 0;
    int copied = 1;
    while (off < names_len) {
        const char *name = names + off;
        size_t name_len = strlen(name);
        if (name_len == 0) {
            copied = 0;
            break;
        }
        ssize_t value_len = getxattr(TMP_FILE, name, value, sizeof(value));
        if (value_len < 0 || setxattr(TMP_DST, name, value, (size_t)value_len, 0) != 0) {
            copied = 0;
            break;
        }
        off += (ssize_t)name_len + 1;
    }

    CHECK(copied, "copyxattr-style loop copies tmpfs attrs");
    check_value(TMP_DST, "user.copy", "payload");
}

static void test_unsupported_rootfs_xattrs(void) {
    CHECK(create_file(ROOT_FILE, "rootfs xattr\n") == 0, "prepare rootfs file");

    CHECK_RET(listxattr(ROOT_FILE, NULL, 0), 0,
              "unsupported rootfs keeps listxattr empty");
    CHECK_ERR(getxattr(ROOT_FILE, "user.root", NULL, 0), ENODATA,
              "unsupported rootfs getxattr returns ENODATA");
    CHECK_ERR(setxattr(ROOT_FILE, "user.root", "x", 1, 0), EOPNOTSUPP,
              "unsupported rootfs setxattr returns EOPNOTSUPP");
    CHECK_ERR(removexattr(ROOT_FILE, "user.root"), EOPNOTSUPP,
              "unsupported rootfs removexattr returns EOPNOTSUPP");
}

int main(void) {
    TEST_START("xattr syscalls");

    test_tmpfs_xattrs();
    test_copyxattr_pattern();
    test_unsupported_rootfs_xattrs();

    unlink(TMP_FILE);
    unlink(TMP_LINK);
    unlink(TMP_DST);
    unlink(ROOT_FILE);

    TEST_DONE();
}
