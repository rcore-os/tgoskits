#define _GNU_SOURCE
#define _FILE_OFFSET_BITS 64

#include "test_framework.h"

#include <fcntl.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

static int call_sync_file_range(int fd, off_t offset, off_t nbytes, unsigned int flags)
{
    return (int)syscall(SYS_sync_file_range, fd, offset, nbytes, flags);
}

int main(void)
{
    TEST_START("sync_file_range Linux 语义对齐");

    /* 测试节点 1：flags=0 是合法 no-op。 */
    {
        char tmpl[] = "/tmp/test-sync-file-range-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 创建普通文件成功");
        if (fd >= 0) {
            CHECK_RET(write(fd, "0123456789", 10), 10, "write 写入测试数据");
            CHECK_RET(call_sync_file_range(fd, 0, 10, 0), 0, "flags=0 返回 0");
            CHECK_RET(call_sync_file_range(fd, 0, 10, SYNC_FILE_RANGE_WRITE), 0,
                      "flags=WRITE 返回 0");
            CHECK_RET(call_sync_file_range(fd, 0, 10,
                                           SYNC_FILE_RANGE_WAIT_BEFORE |
                                               SYNC_FILE_RANGE_WRITE |
                                               SYNC_FILE_RANGE_WAIT_AFTER),
                      0, "WAIT_BEFORE|WRITE|WAIT_AFTER 返回 0");
            CHECK_RET(call_sync_file_range(fd, 0, 0, SYNC_FILE_RANGE_WRITE), 0,
                      "nbytes=0 表示到 EOF，返回 0");
            CHECK_ERR(call_sync_file_range(fd, 0, 10, 0xff), EINVAL,
                      "合法文件 fd + 非法 flags 返回 EINVAL");
            close(fd);
            unlink(tmpl);
        }
    }

    /* 测试节点 2：目录 fd 在 Linux 上允许 sync_file_range。 */
    {
        int fd = open("/tmp", O_RDONLY | O_DIRECTORY);
        CHECK(fd >= 0, "打开 /tmp 目录成功");
        if (fd >= 0) {
            CHECK_RET(call_sync_file_range(fd, 0, 0, SYNC_FILE_RANGE_WRITE), 0,
                      "目录 fd 上 sync_file_range 返回 0");
            close(fd);
        }
    }

    /* 测试节点 3：pipe fd 返回 ESPIPE。 */
    {
        int pipefd[2];
        CHECK_RET(pipe(pipefd), 0, "创建 pipe 成功");
        CHECK_ERR(call_sync_file_range(pipefd[0], 0, 0, SYNC_FILE_RANGE_WRITE), ESPIPE,
                  "pipe 读端 sync_file_range 返回 ESPIPE");
        CHECK_ERR(call_sync_file_range(pipefd[0], 0, 0, 0xff), EINVAL,
                  "pipe 读端 + 非法 flags 时优先返回 EINVAL");
        close(pipefd[0]);
        close(pipefd[1]);
    }

    /* 测试节点 4：非法 fd 返回 EBADF。 */
    CHECK_ERR(call_sync_file_range(-1, 0, 0, SYNC_FILE_RANGE_WRITE), EBADF,
              "fd=-1 时 sync_file_range 返回 EBADF");
    CHECK_ERR(call_sync_file_range(-1, 0, 0, 0xff), EBADF,
              "fd=-1 且 flags 非法时仍优先返回 EBADF");

    /* 测试节点 5：已关闭 fd 返回 EBADF。 */
    {
        char tmpl[] = "/tmp/test-sync-file-range-closed-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 创建关闭场景文件成功");
        if (fd >= 0) {
            close(fd);
            CHECK_ERR(call_sync_file_range(fd, 0, 0, SYNC_FILE_RANGE_WRITE), EBADF,
                      "已关闭 fd 上 sync_file_range 返回 EBADF");
            unlink(tmpl);
        }
    }

    TEST_DONE();
}
