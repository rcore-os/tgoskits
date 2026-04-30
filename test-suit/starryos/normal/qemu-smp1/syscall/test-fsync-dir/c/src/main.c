/*
 * test_fsync_dir.c
 *
 * 测试文件系统修复:
 * 1. fsync 对目录 fd 应返回成功 (Linux 允许)
 * 2. fdatasync 对目录 fd 应返回成功
 * 3. sync_file_range 应返回成功 (建议性优化)
 */

#include "test_framework.h"
#include <fcntl.h>
#include <unistd.h>
#include <sys/syscall.h>
#include <sys/stat.h>

int main(void)
{
    TEST_START("fsync_dir: fsync/fdatasync 目录 + sync_file_range");

    /* Ensure /tmp exists */
    mkdir("/tmp/starry_fsync_test", 0755);

    /* Test 1: fsync on a directory fd */
    {
        int fd = open("/tmp/starry_fsync_test", O_RDONLY | O_DIRECTORY);
        CHECK(fd >= 0, "open directory");
        CHECK_RET(fsync(fd), 0, "fsync on directory fd");
        CHECK_RET(fdatasync(fd), 0, "fdatasync on directory fd");
        close(fd);
    }

    /* Test 2: fsync on a regular file still works */
    {
        int fd = open("/tmp/starry_fsync_test/file", O_RDWR | O_CREAT | O_TRUNC, 0644);
        CHECK(fd >= 0, "create regular file");
        write(fd, "data", 4);
        CHECK_RET(fsync(fd), 0, "fsync on regular file");
        CHECK_RET(fdatasync(fd), 0, "fdatasync on regular file");
        close(fd);
        unlink("/tmp/starry_fsync_test/file");
    }

    /* Test 3: sync_file_range */
    {
        int fd = open("/tmp/starry_fsync_test/sfrfile", O_RDWR | O_CREAT | O_TRUNC, 0644);
        CHECK(fd >= 0, "create file for sync_file_range");
        write(fd, "test data for sync_file_range", 29);

        /* sync_file_range(fd, offset, nbytes, flags)
         * SYNC_FILE_RANGE_WRITE = 2 */
        long rc = syscall(SYS_sync_file_range, fd, 0, 29, 2);
        CHECK(rc == 0, "sync_file_range returns 0");
        close(fd);
        unlink("/tmp/starry_fsync_test/sfrfile");
    }

    rmdir("/tmp/starry_fsync_test");

    TEST_DONE();
}
