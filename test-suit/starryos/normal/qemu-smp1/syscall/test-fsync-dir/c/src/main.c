/*
 * test_fsync_dir.c
 *
 * 测试文件系统修复:
 * 1. fsync 对目录 fd 应返回成功 (Linux 允许)
 * 2. fdatasync 对目录 fd 应返回成功
 * 3. 非法 fd 应返回 EBADF
 * 4. pipe fd 应返回 EINVAL
 * 5. socket fd 应返回 EINVAL
 * 6. sync_file_range 应返回成功 (建议性优化)
 */

#include "test_framework.h"
#include <fcntl.h>
#include <unistd.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/types.h>

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

    /* Test 3: invalid fd handling */
    {
        CHECK_ERR(fsync(-1), EBADF, "fsync on invalid fd -> EBADF");
        CHECK_ERR(fdatasync(-1), EBADF, "fdatasync on invalid fd -> EBADF");
    }

    /* Test 4: fsync on pipe -> EINVAL (Linux behavior) */
    {
        int pfd[2];
        CHECK(pipe(pfd) == 0, "create pipe for fsync EINVAL");
        if (pfd[0] >= 0) {
            CHECK_ERR(fsync(pfd[0]), EINVAL, "fsync on pipe -> EINVAL");
            CHECK_ERR(fdatasync(pfd[0]), EINVAL, "fdatasync on pipe -> EINVAL");
            close(pfd[0]);
        }
        if (pfd[1] >= 0) {
            close(pfd[1]);
        }
    }

    /* Test 5: fsync on socket -> EINVAL (Linux behavior) */
    {
        int sfd[2];
        CHECK(socketpair(AF_UNIX, SOCK_STREAM, 0, sfd) == 0, "create socketpair for fsync EINVAL");
        if (sfd[0] >= 0) {
            CHECK_ERR(fsync(sfd[0]), EINVAL, "fsync on socket -> EINVAL");
            CHECK_ERR(fdatasync(sfd[0]), EINVAL, "fdatasync on socket -> EINVAL");
            close(sfd[0]);
        }
        if (sfd[1] >= 0) {
            close(sfd[1]);
        }
    }

    /* Test 6: sync_file_range */
    {
        int fd = open("/tmp/starry_fsync_test/sfrfile", O_RDWR | O_CREAT | O_TRUNC, 0644);
        CHECK(fd >= 0, "create file for sync_file_range");
        write(fd, "test data for sync_file_range", 29);

        /* sync_file_range(fd, offset, nbytes, flags)
         * SYNC_FILE_RANGE_WRITE = 2 */
        long rc = syscall(SYS_sync_file_range, fd, 0, 29, 2);
        CHECK(rc == 0, "sync_file_range returns 0");
        CHECK_RET(syscall(SYS_sync_file_range, fd, 0, 29, 0), 0,
              "sync_file_range flags==0 -> success");
        CHECK_ERR(syscall(SYS_sync_file_range, fd, 0, 29, 0x8000), EINVAL,
                  "sync_file_range invalid flags -> EINVAL");
        CHECK_ERR(syscall(SYS_sync_file_range, fd, (off_t)-1, 29, 2), EINVAL,
                  "sync_file_range negative offset -> EINVAL");
        CHECK_ERR(syscall(SYS_sync_file_range, fd, 0, (off_t)-1, 2), EINVAL,
              "sync_file_range negative nbytes -> EINVAL");
        close(fd);
        unlink("/tmp/starry_fsync_test/sfrfile");
    }

    rmdir("/tmp/starry_fsync_test");

    TEST_DONE();
}
