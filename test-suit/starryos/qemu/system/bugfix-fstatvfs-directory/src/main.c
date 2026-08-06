#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
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

int main(void)
{
    const char *directory_path = "/tmp/bugfix-fstatvfs-directory";
    const char *file_path = "/tmp/bugfix-fstatvfs-directory/file";
    struct statvfs filesystem;

    printf("=== bugfix-fstatvfs-directory ===\n");
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
