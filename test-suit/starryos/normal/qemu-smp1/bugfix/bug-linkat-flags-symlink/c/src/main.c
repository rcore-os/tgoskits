#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

static int passed;
static int failed;

#define CHECK(cond, msg) \
    do { \
        if (cond) { \
            printf("  [OK] %s\n", (msg)); \
            passed++; \
        } else { \
            printf("  [FAIL] %s (errno=%d %s)\n", (msg), errno, strerror(errno)); \
            failed++; \
        } \
    } while (0)

static void remove_path(const char *path)
{
    unlink(path);
    rmdir(path);
}

static int write_file(const char *path, const char *data)
{
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (fd < 0) {
        return -1;
    }

    size_t len = strlen(data);
    ssize_t written = write(fd, data, len);
    int saved = errno;
    close(fd);
    errno = saved;
    return written == (ssize_t)len ? 0 : -1;
}

static int readlink_string(const char *path, char *buf, size_t len)
{
    ssize_t n = readlink(path, buf, len - 1);
    if (n < 0) {
        return -1;
    }
    buf[n] = '\0';
    return 0;
}

static int read_file_string(const char *path, char *buf, size_t len)
{
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        return -1;
    }
    ssize_t n = read(fd, buf, len - 1);
    int saved = errno;
    close(fd);
    errno = saved;
    if (n < 0) {
        return -1;
    }
    buf[n] = '\0';
    return 0;
}

int main(void)
{
    const char *base = "/tmp/bug_linkat_flags_symlink";
    const char *target = "/tmp/bug_linkat_flags_symlink/target";
    const char *symlink_path = "/tmp/bug_linkat_flags_symlink/symlink";
    const char *symlink_hardlink = "/tmp/bug_linkat_flags_symlink/symlink_hardlink";
    const char *target_hardlink = "/tmp/bug_linkat_flags_symlink/target_hardlink";
    const char *bad_flag_dst = "/tmp/bug_linkat_flags_symlink/bad_flag_dst";
    const char *bad_flag_bad_old_dst =
        "/tmp/bug_linkat_flags_symlink/bad_flag_bad_old_dst";
    const char *existing_dst = "/tmp/bug_linkat_flags_symlink/existing_dst";
    const char *dir_hardlink = "/tmp/bug_linkat_flags_symlink/dir_hardlink";
    const char *bad_user_ptr = (const char *)-1;
    struct stat st;
    char buf[128];

    printf("=== bug-linkat-flags-symlink ===\n");

    remove_path(symlink_hardlink);
    remove_path(target_hardlink);
    remove_path(bad_flag_dst);
    remove_path(bad_flag_bad_old_dst);
    remove_path(existing_dst);
    remove_path(symlink_path);
    remove_path(target);
    remove_path(dir_hardlink);
    remove_path(base);

    CHECK(mkdir(base, 0700) == 0, "mkdir test directory");
    CHECK(write_file(target, "target-data") == 0, "create target file");
    CHECK(symlink("target", symlink_path) == 0, "create relative symlink");

    errno = 0;
    CHECK(linkat(AT_FDCWD, symlink_path, AT_FDCWD, symlink_hardlink, 0) == 0,
          "linkat symlink with flags=0 succeeds");
    CHECK(lstat(symlink_hardlink, &st) == 0 && S_ISLNK(st.st_mode),
          "flags=0 hard link refers to the symlink itself");
    CHECK(readlink_string(symlink_hardlink, buf, sizeof(buf)) == 0 &&
              strcmp(buf, "target") == 0,
          "hard-linked symlink preserves link target text");

    errno = 0;
    CHECK(linkat(AT_FDCWD, symlink_path, AT_FDCWD, target_hardlink,
                 AT_SYMLINK_FOLLOW) == 0,
          "linkat symlink with AT_SYMLINK_FOLLOW succeeds");
    CHECK(lstat(target_hardlink, &st) == 0 && S_ISREG(st.st_mode),
          "AT_SYMLINK_FOLLOW hard link refers to target file");
    CHECK(read_file_string(target_hardlink, buf, sizeof(buf)) == 0 &&
              strcmp(buf, "target-data") == 0,
          "AT_SYMLINK_FOLLOW hard link reads target content");

    errno = 0;
    CHECK(linkat(AT_FDCWD, target, AT_FDCWD, bad_flag_dst, 0x1) == -1 &&
              errno == EINVAL,
          "linkat rejects invalid flag bits with EINVAL");
    CHECK(lstat(bad_flag_dst, &st) == -1 && errno == ENOENT,
          "invalid flags do not create destination");

    errno = 0;
    CHECK(linkat(AT_FDCWD, bad_user_ptr, AT_FDCWD, bad_flag_bad_old_dst, 0x1) ==
                  -1 &&
              errno == EINVAL,
          "linkat invalid flags take precedence over bad old_path");
    CHECK(lstat(bad_flag_bad_old_dst, &st) == -1 && errno == ENOENT,
          "invalid flags with bad old_path do not create destination");

    errno = 0;
    CHECK(linkat(AT_FDCWD, target, AT_FDCWD, bad_user_ptr, 0x1) == -1 &&
              errno == EINVAL,
          "linkat invalid flags take precedence over bad new_path");

    CHECK(write_file(existing_dst, "existing") == 0, "create existing destination");
    errno = 0;
    CHECK(linkat(AT_FDCWD, target, AT_FDCWD, existing_dst, 0) == -1 &&
              errno == EEXIST,
          "linkat existing destination returns EEXIST");

    errno = 0;
    CHECK(linkat(AT_FDCWD, base, AT_FDCWD, dir_hardlink, 0) == -1 &&
              errno == EPERM,
          "linkat directory returns EPERM");

    remove_path(symlink_hardlink);
    remove_path(target_hardlink);
    remove_path(bad_flag_dst);
    remove_path(bad_flag_bad_old_dst);
    remove_path(existing_dst);
    remove_path(symlink_path);
    remove_path(target);
    remove_path(dir_hardlink);
    remove_path(base);

    printf("\n=== result: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("TEST PASSED\n");
    } else {
        printf("TEST FAILED\n");
    }
    return failed == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}
