#define _GNU_SOURCE

#include "test_framework.h"

#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void)
{
    TEST_START("syncfs Linux 语义对齐");

    /* 测试节点 1：普通文件 fd 上 syncfs 返回 0。 */
    {
        char tmpl[] = "/tmp/test-syncfs-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 创建普通文件成功");
        if (fd >= 0) {
            CHECK_RET(syncfs(fd), 0, "普通文件 fd 上 syncfs 返回 0");
            close(fd);
            unlink(tmpl);
        }
    }

    /* 测试节点 2：目录 fd 也可以作为所在文件系统的锚点。 */
    {
        int fd = open("/tmp", O_RDONLY | O_DIRECTORY);
        CHECK(fd >= 0, "打开 /tmp 目录成功");
        if (fd >= 0) {
            CHECK_RET(syncfs(fd), 0, "目录 fd 上 syncfs 返回 0");
            close(fd);
        }
    }

    /*
     * 测试节点 3：pipe fd 在 Linux 上同样返回 0。
     * 原因是 syncfs 只要求“合法的打开 fd”，pipe 也属于 pipefs。
     */
    {
        int pipefd[2];
        CHECK_RET(pipe(pipefd), 0, "创建 pipe 成功");
        CHECK_RET(syncfs(pipefd[0]), 0, "pipe 读端 syncfs 返回 0");
        close(pipefd[0]);
        close(pipefd[1]);
    }

    /* 测试节点 4：非法 fd 应返回 EBADF。 */
    CHECK_ERR(syncfs(-1), EBADF, "fd=-1 时 syncfs 返回 EBADF");

    /* 测试节点 5：已关闭 fd 应返回 EBADF。 */
    {
        char tmpl[] = "/tmp/test-syncfs-closed-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 创建关闭场景文件成功");
        if (fd >= 0) {
            close(fd);
            CHECK_ERR(syncfs(fd), EBADF, "已关闭 fd 上 syncfs 返回 EBADF");
            unlink(tmpl);
        }
    }

    TEST_DONE();
}
