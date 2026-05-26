#define _GNU_SOURCE

#include "test_framework.h"

#include <fcntl.h>
#include <string.h>
#include <unistd.h>

int main(void)
{
    TEST_START("sync Linux 语义对齐");

    /*
     * 测试节点 1：sync 没有返回值。
     * 这里验证它不会中断后续执行，并且调用后文件内容仍可正常读回。
     */
    {
        char tmpl[] = "/tmp/test-sync-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 创建普通文件成功");
        if (fd >= 0) {
            const char *msg = "sync-data";
            char buf[16] = {0};

            CHECK_RET(write(fd, msg, strlen(msg)), (long)strlen(msg), "write 写入 sync-data");
            sync();
            CHECK(1, "sync 调用返回到用户态");
            CHECK_RET(lseek(fd, 0, SEEK_SET), 0, "lseek 回到文件头");
            CHECK_RET(read(fd, buf, strlen(msg)), (long)strlen(msg), "read 读回写入内容");
            CHECK(strcmp(buf, msg) == 0, "sync 后读回内容与写入一致");

            close(fd);
            unlink(tmpl);
        }
    }

    /* 测试节点 2：sync 对目录项创建路径不应造成异常。 */
    {
        char path[] = "/tmp/test-sync-path-XXXXXX";
        int fd = mkstemp(path);
        CHECK(fd >= 0, "mkstemp 创建路径场景文件成功");
        if (fd >= 0) {
            close(fd);
            sync();
            CHECK(access(path, F_OK) == 0, "sync 后刚创建的路径仍可访问");
            unlink(path);
        }
    }

    TEST_DONE();
}
