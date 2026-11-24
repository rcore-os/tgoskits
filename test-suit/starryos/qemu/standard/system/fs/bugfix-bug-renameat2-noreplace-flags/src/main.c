#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef RENAME_NOREPLACE
#define RENAME_NOREPLACE (1U << 0)
#endif

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

static int renameat2_sys(const char *old_path, const char *new_path,
                         unsigned int flags)
{
    return (int)syscall(SYS_renameat2, AT_FDCWD, old_path, AT_FDCWD, new_path,
                        flags);
}

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

static void cleanup(const char *base, const char *old_path, const char *new_path,
                    const char *target_path)
{
    remove_path(old_path);
    remove_path(new_path);
    remove_path(target_path);
    remove_path(base);
}

int main(void)
{
    const char *base = "/tmp/bug_renameat2_noreplace_flags";
    const char *old_path = "/tmp/bug_renameat2_noreplace_flags/old";
    const char *new_path = "/tmp/bug_renameat2_noreplace_flags/new";
    const char *target_path = "/tmp/bug_renameat2_noreplace_flags/target";
    const char *bad_user_ptr = (const char *)-1;
    char buf[64];
    struct stat st;

    printf("=== bug-renameat2-noreplace-flags ===\n");

    cleanup(base, old_path, new_path, target_path);
    CHECK(mkdir(base, 0700) == 0, "mkdir test directory");

    CHECK(write_file(old_path, "old-data") == 0, "create old file");
    CHECK(write_file(new_path, "new-data") == 0, "create existing destination");
    errno = 0;
    CHECK(renameat2_sys(old_path, new_path, RENAME_NOREPLACE) == -1 &&
              errno == EEXIST,
          "RENAME_NOREPLACE rejects existing destination with EEXIST");
    CHECK(read_file_string(old_path, buf, sizeof(buf)) == 0 &&
              strcmp(buf, "old-data") == 0,
          "old file remains after RENAME_NOREPLACE EEXIST");
    CHECK(read_file_string(new_path, buf, sizeof(buf)) == 0 &&
              strcmp(buf, "new-data") == 0,
          "destination remains after RENAME_NOREPLACE EEXIST");

    cleanup(base, old_path, new_path, target_path);
    CHECK(mkdir(base, 0700) == 0, "recreate test directory for missing source");
    CHECK(write_file(new_path, "new-data") == 0,
          "create destination before missing-source rename");
    errno = 0;
    CHECK(renameat2_sys(old_path, new_path, RENAME_NOREPLACE) == -1 &&
              errno == ENOENT,
          "RENAME_NOREPLACE reports ENOENT when source leaf is missing");
    CHECK(read_file_string(new_path, buf, sizeof(buf)) == 0 &&
              strcmp(buf, "new-data") == 0,
          "missing-source no-replace leaves destination unchanged");

    cleanup(base, old_path, new_path, target_path);
    CHECK(mkdir(base, 0700) == 0, "recreate test directory for symlink target");
    CHECK(write_file(old_path, "old-data") == 0, "create source for symlink target");
    CHECK(write_file(target_path, "target-data") == 0,
          "create existing symlink target");
    CHECK(symlink(target_path, new_path) == 0, "create destination symlink");
    errno = 0;
    CHECK(renameat2_sys(old_path, new_path, RENAME_NOREPLACE) == -1 &&
              errno == EEXIST,
          "RENAME_NOREPLACE rejects destination symlink with EEXIST");
    CHECK(lstat(new_path, &st) == 0 && S_ISLNK(st.st_mode),
          "destination symlink remains a symlink after EEXIST");
    CHECK(read_file_string(old_path, buf, sizeof(buf)) == 0 &&
              strcmp(buf, "old-data") == 0,
          "source remains after symlink destination EEXIST");
    CHECK(read_file_string(target_path, buf, sizeof(buf)) == 0 &&
              strcmp(buf, "target-data") == 0,
          "symlink target remains unchanged after EEXIST");

    cleanup(base, old_path, new_path, target_path);
    CHECK(mkdir(base, 0700) == 0,
          "recreate test directory for dangling symlink target");
    CHECK(write_file(old_path, "old-data") == 0,
          "create source for dangling symlink target");
    CHECK(symlink(target_path, new_path) == 0,
          "create dangling destination symlink");
    errno = 0;
    CHECK(renameat2_sys(old_path, new_path, RENAME_NOREPLACE) == -1 &&
              errno == EEXIST,
          "RENAME_NOREPLACE treats dangling destination symlink as existing");
    CHECK(lstat(new_path, &st) == 0 && S_ISLNK(st.st_mode),
          "dangling destination symlink remains after EEXIST");
    CHECK(read_file_string(old_path, buf, sizeof(buf)) == 0 &&
              strcmp(buf, "old-data") == 0,
          "source remains after dangling symlink EEXIST");
    errno = 0;
    CHECK(stat(new_path, &st) == -1 && errno == ENOENT,
          "dangling destination symlink target is still absent");

    cleanup(base, old_path, new_path, target_path);
    CHECK(mkdir(base, 0700) == 0, "recreate test directory");
    CHECK(write_file(old_path, "move-data") == 0, "create source for no-replace success");
    errno = 0;
    CHECK(renameat2_sys(old_path, new_path, RENAME_NOREPLACE) == 0,
          "RENAME_NOREPLACE succeeds when destination is absent");
    CHECK(stat(old_path, &st) == -1 && errno == ENOENT,
          "old file disappears after successful no-replace rename");
    CHECK(read_file_string(new_path, buf, sizeof(buf)) == 0 &&
              strcmp(buf, "move-data") == 0,
          "destination receives source content after no-replace rename");

    cleanup(base, old_path, new_path, target_path);
    CHECK(mkdir(base, 0700) == 0, "recreate test directory for invalid flags");
    CHECK(write_file(old_path, "invalid-data") == 0, "create source for invalid flags");
    errno = 0;
    CHECK(renameat2_sys(old_path, new_path, 0x80000000U) == -1 &&
              errno == EINVAL,
          "renameat2 rejects unknown flags with EINVAL");
    CHECK(read_file_string(old_path, buf, sizeof(buf)) == 0 &&
              strcmp(buf, "invalid-data") == 0,
          "invalid flags leave source file in place");
    CHECK(stat(new_path, &st) == -1 && errno == ENOENT,
          "invalid flags do not create destination");

    errno = 0;
    CHECK(renameat2_sys(bad_user_ptr, new_path, 0x80000000U) == -1 &&
              errno == EINVAL,
          "renameat2 invalid flags take precedence over bad old_path");
    CHECK(stat(new_path, &st) == -1 && errno == ENOENT,
          "invalid flags with bad old_path do not create destination");

    errno = 0;
    CHECK(renameat2_sys(old_path, bad_user_ptr, 0x80000000U) == -1 &&
              errno == EINVAL,
          "renameat2 invalid flags take precedence over bad new_path");
    CHECK(read_file_string(old_path, buf, sizeof(buf)) == 0 &&
              strcmp(buf, "invalid-data") == 0,
          "invalid flags with bad new_path leave source file in place");

    cleanup(base, old_path, new_path, target_path);
    CHECK(mkdir(base, 0700) == 0, "recreate test directory for flags=0");
    CHECK(write_file(old_path, "replace-data") == 0, "create source for normal rename");
    CHECK(write_file(new_path, "existing-data") == 0, "create target for normal rename");
    errno = 0;
    CHECK(renameat2_sys(old_path, new_path, 0) == 0,
          "renameat2 flags=0 replaces existing destination");
    CHECK(read_file_string(new_path, buf, sizeof(buf)) == 0 &&
              strcmp(buf, "replace-data") == 0,
          "flags=0 replacement writes source content to destination");

    cleanup(base, old_path, new_path, target_path);

    printf("\n=== result: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("TEST PASSED\n");
    } else {
        printf("TEST FAILED\n");
    }
    return failed == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}
