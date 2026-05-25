#define _GNU_SOURCE

#include "test_framework.h"

#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void)
{
    TEST_START("fdatasync Linux 语义对齐");

    /* 测试节点 1：普通文件写入后 fdatasync 应返回 0。 */
    {
        char tmpl[] = "/tmp/test-fdatasync-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 创建普通文件成功");
        if (fd >= 0) {
            CHECK_RET(write(fd, "datasync", 8), 8, "write 写入 8 字节");
            CHECK_RET(fdatasync(fd), 0, "普通文件 fdatasync 返回 0");
            close(fd);
            unlink(tmpl);
        }
    }

    /* 测试节点 2：只读打开的普通文件也允许 fdatasync。 */
    {
        char tmpl[] = "/tmp/test-fdatasync-ro-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 创建只读场景文件成功");
        if (fd >= 0) {
            close(fd);
            fd = open(tmpl, O_RDONLY);
            CHECK(fd >= 0, "O_RDONLY 重新打开文件成功");
            if (fd >= 0) {
                CHECK_RET(fdatasync(fd), 0, "只读 fd 上 fdatasync 返回 0");
                close(fd);
            }
            unlink(tmpl);
        }
    }

    /* 测试节点 3：目录 fd 在 Linux 上允许 fdatasync。 */
    {
        int fd = open("/tmp", O_RDONLY | O_DIRECTORY);
        CHECK(fd >= 0, "打开 /tmp 目录成功");
        if (fd >= 0) {
            CHECK_RET(fdatasync(fd), 0, "目录 fd 上 fdatasync 返回 0");
            close(fd);
        }
    }

    /* 测试节点 4：pipe 不支持 fdatasync，应返回 EINVAL。 */
    {
        int pipefd[2];
        CHECK_RET(pipe(pipefd), 0, "创建 pipe 成功");
        CHECK_ERR(fdatasync(pipefd[0]), EINVAL, "pipe 读端 fdatasync 返回 EINVAL");
        close(pipefd[0]);
        close(pipefd[1]);
    }

    /* 测试节点 5：非法 fd 应返回 EBADF。 */
    CHECK_ERR(fdatasync(-1), EBADF, "fd=-1 时 fdatasync 返回 EBADF");

    /* 测试节点 6：已关闭 fd 应返回 EBADF。 */
    {
        char tmpl[] = "/tmp/test-fdatasync-closed-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 创建关闭场景文件成功");
        if (fd >= 0) {
            close(fd);
            CHECK_ERR(fdatasync(fd), EBADF, "已关闭 fd 上 fdatasync 返回 EBADF");
            unlink(tmpl);
        }
    }

    TEST_DONE();
}
