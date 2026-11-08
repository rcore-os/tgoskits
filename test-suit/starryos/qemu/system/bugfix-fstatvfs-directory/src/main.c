#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <sys/syscall.h>
#include <sys/vfs.h>
#include <stdint.h>
#include <unistd.h>

static int passed;
static int failed;

static void expect_true(int condition, const char *name)
{
    if (condition) {
        printf("PASS: %s\n", name);
        passed++;
        return;
    }
    printf("FAIL: %s: errno=%d (%s)\n", name, errno, strerror(errno));
    failed++;
}

/* The path and descriptor must observe one published root geometry, including
 * when boot preparation has expanded the underlying ext4 filesystem. */
static void verify_root_filesystem_statistics(void)
{
    struct statfs by_path, by_fd;
    int root_fd = open("/", O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    expect_true(root_fd >= 0, "open root directory fd");
    if (root_fd < 0) {
        return;
    }
    int path_result = syscall(SYS_statfs, "/", &by_path);
    expect_true(path_result == 0, "statfs reports root filesystem");
    int fd_result = syscall(SYS_fstatfs, root_fd, &by_fd);
    expect_true(fd_result == 0, "fstatfs reports root filesystem");
    close(root_fd);
    if (path_result != 0 || fd_result != 0) {
        return;
    }
    expect_true(by_path.f_type == by_fd.f_type
                && by_path.f_bsize == by_fd.f_bsize
                && by_path.f_blocks == by_fd.f_blocks,
                "root path and fd report the same geometry");
    /* Free counters are independent snapshots; another process may allocate
     * between these calls without changing the published filesystem geometry. */
    expect_true(by_path.f_bsize > 0 && by_path.f_blocks > 0
                && by_path.f_bfree <= by_path.f_blocks
                && by_path.f_bavail <= by_path.f_bfree,
                "root filesystem capacity is consistent");
    expect_true(by_fd.f_bsize > 0 && by_fd.f_blocks > 0
                && by_fd.f_bfree <= by_fd.f_blocks
                && by_fd.f_bavail <= by_fd.f_bfree,
                "root fd capacity is consistent");
    if (by_path.f_bsize <= 0 || by_path.f_blocks > UINT64_MAX / by_path.f_bsize) {
        expect_true(0, "root filesystem byte size is representable");
        return;
    }
    const uint64_t capacity = (uint64_t)by_path.f_blocks * by_path.f_bsize;
    printf("STARRY_ROOT_STATFS_BYTES=%llu\n", (unsigned long long)capacity);
    const char *expected = getenv("STARRY_ROOT_EXPECTED_BYTES");
    if (expected != NULL) {
        char *end;
        errno = 0;
        unsigned long long target = strtoull(expected, &end, 10);
        expect_true(errno == 0 && *expected != '\0' && *end == '\0'
                    && target > 0 && capacity == target,
                    "boot growth publishes the enlarged root capacity");
    }
}

int main(void)
{
    const char *directory_path = "/tmp/bugfix-fstatvfs-directory";
    const char *file_path = "/tmp/bugfix-fstatvfs-directory/file";
    struct statvfs filesystem;

    printf("=== bugfix-fstatvfs-directory ===\n");
    verify_root_filesystem_statistics();
    mkdir(directory_path, 0700);

    int directory_fd = open(directory_path, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    expect_true(directory_fd >= 0, "open directory fd");
    if (directory_fd >= 0) {
        errno = 0;
        int result = fstatvfs(directory_fd, &filesystem);
        expect_true(result == 0, "fstatvfs accepts directory fd");
        if (result == 0) {
            expect_true(filesystem.f_bsize > 0,
                        "directory fstatvfs reports block size");
        }
        close(directory_fd);
    }

    int file_fd = open(file_path, O_CREAT | O_RDWR | O_TRUNC | O_CLOEXEC, 0600);
    expect_true(file_fd >= 0, "open regular file fd");
    if (file_fd >= 0) {
        errno = 0;
        expect_true(fstatvfs(file_fd, &filesystem) == 0,
                    "fstatvfs accepts regular file fd");
        close(file_fd);
    }

    errno = 0;
    expect_true(fstatvfs(-1, &filesystem) == -1 && errno == EBADF,
                "fstatvfs rejects invalid fd with EBADF");

    unlink(file_path);
    rmdir(directory_path);

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        puts("STARRY_FSTATVFS_DIRECTORY_PASSED");
        puts("STARRY_GROUPED_TEST_PASSED: bugfix-fstatvfs-directory");
        return EXIT_SUCCESS;
    }
    puts("STARRY_GROUPED_TEST_FAILED: bugfix-fstatvfs-directory");
    return EXIT_FAILURE;
}
