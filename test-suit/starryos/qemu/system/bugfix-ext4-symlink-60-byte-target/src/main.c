#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef AT_EMPTY_PATH
#define AT_EMPTY_PATH 0x1000
#endif
#ifndef STATX_TYPE
#define STATX_TYPE 0x00000001U
#endif
#ifndef STATX_SIZE
#define STATX_SIZE 0x00000200U
#endif
#ifndef STATX_BLOCKS
#define STATX_BLOCKS 0x00000400U
#endif

#define FIXTURE_DIR \
    "/usr/share/starry-test-suit/ext4-symlink-60-byte-target"

struct statx_timestamp_abi {
    int64_t tv_sec;
    uint32_t tv_nsec;
    int32_t reserved;
};

struct statx_abi {
    uint32_t stx_mask;
    uint32_t stx_blksize;
    uint64_t stx_attributes;
    uint32_t stx_nlink;
    uint32_t stx_uid;
    uint32_t stx_gid;
    uint16_t stx_mode;
    uint16_t pad0;
    uint64_t stx_ino;
    uint64_t stx_size;
    uint64_t stx_blocks;
    uint64_t stx_attributes_mask;
    struct statx_timestamp_abi stx_atime;
    struct statx_timestamp_abi stx_btime;
    struct statx_timestamp_abi stx_ctime;
    struct statx_timestamp_abi stx_mtime;
    uint32_t stx_rdev_major;
    uint32_t stx_rdev_minor;
    uint32_t stx_dev_major;
    uint32_t stx_dev_minor;
    uint64_t stx_mnt_id;
    uint64_t spare[13];
};

static int passed;
static int failed;

static void check(int condition, const char *message)
{
    if (condition) {
        printf("PASS: %s\n", message);
        passed++;
        return;
    }

    printf("FAIL: %s: errno=%d (%s)\n", message, errno, strerror(errno));
    failed++;
}

static void make_expected_target(char *target, size_t target_len, char fill)
{
    target[0] = '/';
    memset(target + 1, fill, target_len - 1);
    target[target_len] = '\0';
}

static long raw_statx(int dirfd, const char *path, int flags, unsigned int mask,
                      struct statx_abi *status)
{
    return syscall(SYS_statx, dirfd, path, flags, mask, status);
}

static void test_symlink(int dirfd, const char *name, size_t target_len,
                         char fill, int expect_fast_symlink,
                         int verify_block_encoding)
{
    char message[160];
    char expected[64];
    char actual[64];
    struct stat status;
    struct statx_abi status_x;

    make_expected_target(expected, target_len, fill);

    errno = 0;
    int result = fstatat(dirfd, name, &status, AT_SYMLINK_NOFOLLOW);
    snprintf(message, sizeof(message), "%zu-byte fixture is a symlink",
             target_len);
    check(result == 0 && S_ISLNK(status.st_mode), message);
    if (result != 0) {
        return;
    }

    if (verify_block_encoding) {
        snprintf(message, sizeof(message),
                 "%zu-byte fixture uses the expected ext4 block encoding",
                 target_len);
        check(expect_fast_symlink ? status.st_blocks == 0 : status.st_blocks > 0,
              message);
    }

    errno = 0;
    int path_fd = openat(dirfd, name, O_PATH | O_NOFOLLOW | O_CLOEXEC);
    snprintf(message, sizeof(message),
             "openat O_PATH|O_NOFOLLOW accepts %zu-byte symlink", target_len);
    check(path_fd >= 0, message);
    if (path_fd < 0) {
        return;
    }

    memset(&status_x, 0, sizeof(status_x));
    errno = 0;
    result = raw_statx(path_fd, "", AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW,
                       STATX_TYPE | STATX_SIZE | STATX_BLOCKS, &status_x);
    snprintf(message, sizeof(message),
             "statx AT_EMPTY_PATH reports %zu-byte symlink", target_len);
    check(result == 0 && S_ISLNK(status_x.stx_mode) &&
              status_x.stx_size == target_len,
          message);
    close(path_fd);

    memset(actual, 0, sizeof(actual));
    errno = 0;
    ssize_t bytes = readlinkat(dirfd, name, actual, sizeof(actual));
    snprintf(message, sizeof(message),
             "readlinkat returns exact %zu-byte target", target_len);
    check(bytes == (ssize_t)target_len &&
              memcmp(actual, expected, target_len) == 0,
          message);
}

int main(void)
{
    const char *fixture_dir = getenv("STARRY_SYMLINK_FIXTURE_DIR");
    const char *verify_encoding = getenv("STARRY_VERIFY_EXT4_ENCODING");
    int verify_block_encoding =
        verify_encoding == NULL || strcmp(verify_encoding, "0") != 0;

    if (fixture_dir == NULL || fixture_dir[0] == '\0') {
        fixture_dir = FIXTURE_DIR;
    }

    printf("STARRY_SYSTEM_TEST_BEGIN: bugfix-ext4-symlink-60-byte-target\n");

    errno = 0;
    int dirfd = open(fixture_dir, O_PATH | O_DIRECTORY | O_CLOEXEC);
    check(dirfd >= 0, "open host-created ext4 symlink fixture directory");
    if (dirfd >= 0) {
        if (!verify_block_encoding) {
            printf("SKIP: ext4 block encoding check disabled for host oracle\n");
        }
        test_symlink(dirfd, "link-59", 59, 'a', 1, verify_block_encoding);
        test_symlink(dirfd, "link-60", 60, 'b', 0, verify_block_encoding);
        test_symlink(dirfd, "link-61", 61, 'c', 0, verify_block_encoding);
        close(dirfd);
    }

    printf("Results: pass=%d fail=%d\n", passed, failed);
    if (failed == 0) {
        printf("STARRY_GROUPED_TEST_PASSED: "
               "bugfix-ext4-symlink-60-byte-target\n");
        return EXIT_SUCCESS;
    }

    printf("STARRY_GROUPED_TEST_FAILED: "
           "bugfix-ext4-symlink-60-byte-target\n");
    return EXIT_FAILURE;
}
