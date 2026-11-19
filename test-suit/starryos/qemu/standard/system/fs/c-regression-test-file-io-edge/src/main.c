/*
 * test_file_io_edge.c -- ftruncate/pwrite64/lseek 负值参数测试
 *
 * 验证：
 *   1. ftruncate: 负 length 应返回 EINVAL
 *   2. pwrite64: 负 offset 应返回 EINVAL
 *   3. lseek SEEK_SET: 负 offset 应返回 EINVAL
 */

#include "test_framework.h"
#include <fcntl.h>
#include <unistd.h>
#include <sys/types.h>
#include <string.h>
#include <errno.h>

#define TEST_FILE "/tmp/file_io_edge_test"

static int create_test_file(void)
{
    int fd = open(TEST_FILE, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd >= 0) {
        if (write(fd, "hello world\n", 12) != 12) {
            close(fd);
            return -1;
        }
    }
    return fd;
}

int main(void)
{
    TEST_START("file_io_edge: ftruncate/pwrite64/lseek 负值参数测试");

    /* ftruncate 正常截断 */
    int fd = create_test_file();
    CHECK(fd >= 0, "ftruncate 正常截断: 打开文件");
    if (fd >= 0) {
        CHECK_RET(ftruncate(fd, 5), 0, "ftruncate(5) 成功");
        CHECK_RET(lseek(fd, 0, SEEK_END), 5, "ftruncate(5) 后文件大小为 5");
        close(fd);
    }
    unlink(TEST_FILE);

    /* ftruncate 负 length 应返回 EINVAL
     * Linux fs/open.c do_truncate(): if (length < 0) return -EINVAL */
    fd = create_test_file();
    CHECK(fd >= 0, "ftruncate 负值测试: 打开文件");
    if (fd >= 0) {
        CHECK_ERR(ftruncate(fd, -1), EINVAL, "ftruncate(-1) 返回 EINVAL");
        close(fd);
    }
    unlink(TEST_FILE);

    /* ftruncate 截断为 0 */
    fd = create_test_file();
    CHECK(fd >= 0, "ftruncate 截断为 0: 打开文件");
    if (fd >= 0) {
        CHECK_RET(ftruncate(fd, 0), 0, "ftruncate(0) 成功");
        CHECK_RET(lseek(fd, 0, SEEK_END), 0, "ftruncate(0) 后文件大小为 0");
        close(fd);
    }
    unlink(TEST_FILE);

    /* pwrite 正常写入 */
    fd = create_test_file();
    CHECK(fd >= 0, "pwrite 正常写入: 打开文件");
    if (fd >= 0) {
        CHECK_RET(pwrite(fd, "XY", 2, 5), 2, "pwrite(fd, \"XY\", 2, 5) 写入 2 字节");
        char buf[13] = {0};
        CHECK_RET(pread(fd, buf, 12, 0), 12, "pread 读回 12 字节");
        CHECK(memcmp(buf, "helloXYorld\n", 12) == 0, "pwrite 后偏移 5 处为 XY");
        close(fd);
    }
    unlink(TEST_FILE);

    /* pwrite 负 offset 应返回 EINVAL
     * Linux fs/read_write.c ksys_pwrite64(): if (pos < 0) return -EINVAL */
    fd = create_test_file();
    CHECK(fd >= 0, "pwrite 负 offset 测试: 打开文件");
    if (fd >= 0) {
        CHECK_ERR(pwrite(fd, "X", 1, -1), EINVAL, "pwrite(fd, \"X\", 1, -1) 返回 EINVAL");
        close(fd);
    }
    unlink(TEST_FILE);

    /* pread 负 offset 应返回 EINVAL（验证已有修复） */
    fd = create_test_file();
    CHECK(fd >= 0, "pread 负 offset 测试: 打开文件");
    if (fd >= 0) {
        char buf[16];
        CHECK_ERR(pread(fd, buf, 1, -1), EINVAL, "pread(fd, buf, 1, -1) 返回 EINVAL");
        close(fd);
    }
    unlink(TEST_FILE);

    /* lseek SEEK_SET 负 offset 应返回 EINVAL */
    fd = create_test_file();
    CHECK(fd >= 0, "lseek 负 offset 测试: 打开文件");
    if (fd >= 0) {
        CHECK_ERR(lseek(fd, -1, SEEK_SET), EINVAL, "lseek(fd, -1, SEEK_SET) 返回 EINVAL");
        close(fd);
    }
    unlink(TEST_FILE);

    /* lseek SEEK_SET 正常使用 */
    fd = create_test_file();
    CHECK(fd >= 0, "lseek SEEK_SET 正常: 打开文件");
    if (fd >= 0) {
        CHECK_RET(lseek(fd, 5, SEEK_SET), 5, "lseek(fd, 5, SEEK_SET) == 5");
        CHECK_RET(lseek(fd, 0, SEEK_END), 12, "lseek(fd, 0, SEEK_END) == 12");
        close(fd);
    }
    unlink(TEST_FILE);

    /* lseek 无效 whence 返回 EINVAL */
    fd = create_test_file();
    CHECK(fd >= 0, "lseek 无效 whence 测试: 打开文件");
    if (fd >= 0) {
        CHECK_ERR(lseek(fd, 0, 99), EINVAL, "lseek(fd, 0, 99) 返回 EINVAL");
        close(fd);
    }
    unlink(TEST_FILE);

    TEST_DONE();
}
