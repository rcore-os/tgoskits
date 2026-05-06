/*
 * test_copy_file_range.c — 验证 copy_file_range 系统调用的参数校验及基本功能。
 *
 * 覆盖场景：
 *   1. 文件间基本拷贝，验证偏移量更新和数据正确性
 *   2. 带偏移量的拷贝
 *   3. flags 非零返回 EINVAL（Linux 规定 flags 必须为 0）
 *   4. 同文件重叠拷贝返回 EINVAL
 *   5. len=0 返回 0
 *   6. 超出文件末尾拷贝返回 0
 *   7. 管道 fd 传入返回 EINVAL
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include "test_framework.h"
#include <errno.h>
#include <fcntl.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

/* musl 可能不提供 copy_file_range 封装，直接走 syscall */
static ssize_t my_copy_file_range(int fd_in, off_t *off_in,
                                   int fd_out, off_t *off_out,
                                   size_t len, unsigned int flags) {
    return syscall(SYS_copy_file_range, fd_in, off_in, fd_out, off_out, len, flags);
}

#define TEST_SRC "/tmp/starry_test_cfr_src"
#define TEST_DST "/tmp/starry_test_cfr_dst"
#define TEST_SAME "/tmp/starry_test_cfr_same"

int main(void)
{
    TEST_START("copy_file_range");

    /* 正向测试: 文件间基本拷贝，验证偏移量和数据 */
    {
        int src = open(TEST_SRC, O_RDWR | O_CREAT | O_TRUNC, 0644);
        CHECK(src >= 0, "打开源文件");
        if (src < 0) goto cleanup_basic;

        const char *data = "Hello, copy_file_range!";
        ssize_t len = (ssize_t)strlen(data);
        CHECK_RET(write(src, data, len), len, "写入源文件");

        int dst = open(TEST_DST, O_RDWR | O_CREAT | O_TRUNC, 0644);
        CHECK(dst >= 0, "打开目标文件");
        if (dst < 0) { close(src); goto cleanup_basic; }

        off_t off_in = 0;
        off_t off_out = 0;
        ssize_t n = my_copy_file_range(src, &off_in, dst, &off_out, len, 0);
        CHECK(n == len, "拷贝字节数正确");

        /* 偏移量应随拷贝量前进 */
        CHECK(off_in == (off_t)len, "off_in 更新正确");
        CHECK(off_out == (off_t)len, "off_out 更新正确");

        /* 读回验证数据一致 */
        char buf[64];
        memset(buf, 0, sizeof(buf));
        CHECK_RET(pread(dst, buf, len, 0), len, "读回目标文件");
        CHECK(memcmp(buf, data, len) == 0, "目标文件内容正确");

        close(src);
        close(dst);
    cleanup_basic:
        unlink(TEST_SRC);
        unlink(TEST_DST);
    }

    /* 带偏移量的拷贝：从 src 中间拷贝到 dst 中间 */
    {
        int src = open(TEST_SRC, O_RDWR | O_CREAT | O_TRUNC, 0644);
        CHECK(src >= 0, "打开源文件（偏移测试）");
        if (src < 0) goto cleanup_offsets;

        CHECK_RET(write(src, "AAAA_HELLO_BBBB", 15), 15, "写入源文件（偏移测试）");

        int dst = open(TEST_DST, O_RDWR | O_CREAT | O_TRUNC, 0644);
        CHECK(dst >= 0, "打开目标文件（偏移测试）");
        if (dst < 0) { close(src); goto cleanup_offsets; }

        CHECK_RET(write(dst, "XXXXXXXXXXXXXXX", 15), 15, "写入目标文件（偏移测试）");

        off_t off_in = 5;   /* 从 "HELLO" 开始 */
        off_t off_out = 3;  /* 写入 dst[3] 起始 */
        ssize_t n = my_copy_file_range(src, &off_in, dst, &off_out, 5, 0);
        CHECK(n == 5, "偏移拷贝字节数正确");

        /* dst 应变为 "XXXHELLOXXXXXXX" */
        char buf[32];
        memset(buf, 0, sizeof(buf));
        CHECK_RET(pread(dst, buf, 15, 0), 15, "读回目标文件（偏移测试）");
        CHECK(memcmp(buf, "XXXHELLOXXXXXXX", 15) == 0, "偏移拷贝目标内容正确");

        close(src);
        close(dst);
    cleanup_offsets:
        unlink(TEST_SRC);
        unlink(TEST_DST);
    }

    /* Linux 规定 flags 必须为 0，非零值应返回 EINVAL */
    {
        int src = open(TEST_SRC, O_RDWR | O_CREAT | O_TRUNC, 0644);
        CHECK(src >= 0, "打开源文件（flags 测试）");
        if (src < 0) goto cleanup_flags;

        CHECK_RET(write(src, "testdata", 8), 8, "写入源文件（flags 测试）");

        int dst = open(TEST_DST, O_RDWR | O_CREAT | O_TRUNC, 0644);
        CHECK(dst >= 0, "打开目标文件（flags 测试）");
        if (dst < 0) { close(src); goto cleanup_flags; }

        off_t off_in = 0;
        off_t off_out = 0;
        /* flags=1 应被拒绝 */
        ssize_t n = my_copy_file_range(src, &off_in, dst, &off_out, 8, 1);
        CHECK(n == -1 && errno == EINVAL, "flags=1 应返回 EINVAL");

        close(src);
        close(dst);
    cleanup_flags:
        unlink(TEST_SRC);
        unlink(TEST_DST);
    }

    /* 同文件重叠拷贝：Linux 语义要求返回 EINVAL */
    {
        int fd = open(TEST_SAME, O_RDWR | O_CREAT | O_TRUNC, 0644);
        CHECK(fd >= 0, "打开同文件（重叠测试）");
        if (fd < 0) goto cleanup_overlap;

        unsigned char pattern[8192];
        for (size_t i = 0; i < 8192; i++)
            pattern[i] = (unsigned char)(i & 0xFF);
        CHECK_RET(write(fd, pattern, 8192), 8192, "写入同文件测试数据");

        /* 从 offset 0 拷贝 6000 字节到 offset 2000（同文件前向重叠） */
        off_t off_in = 0;
        off_t off_out = 2000;
        CHECK_ERR(my_copy_file_range(fd, &off_in, fd, &off_out, 6000, 0),
                  EINVAL, "同文件重叠拷贝应返回 EINVAL");

        close(fd);
    cleanup_overlap:
        unlink(TEST_SAME);
    }

    /* len=0 应成功返回 0 */
    {
        int src = open(TEST_SRC, O_RDWR | O_CREAT | O_TRUNC, 0644);
        CHECK(src >= 0, "打开源文件（零长度测试）");
        if (src < 0) goto cleanup_zero;

        CHECK_RET(write(src, "data", 4), 4, "写入源文件（零长度测试）");

        int dst = open(TEST_DST, O_RDWR | O_CREAT | O_TRUNC, 0644);
        CHECK(dst >= 0, "打开目标文件（零长度测试）");
        if (dst < 0) { close(src); goto cleanup_zero; }

        off_t off_in = 0;
        off_t off_out = 0;
        CHECK_RET(my_copy_file_range(src, &off_in, dst, &off_out, 0, 0), 0, "len=0 返回 0");

        close(src);
        close(dst);
    cleanup_zero:
        unlink(TEST_SRC);
        unlink(TEST_DST);
    }

    /* 从文件末尾之后读取，应返回 0（无数据） */
    {
        int src = open(TEST_SRC, O_RDWR | O_CREAT | O_TRUNC, 0644);
        CHECK(src >= 0, "打开源文件（越界测试）");
        if (src < 0) goto cleanup_eof;

        CHECK_RET(write(src, "short", 5), 5, "写入源文件（越界测试）");

        int dst = open(TEST_DST, O_RDWR | O_CREAT | O_TRUNC, 0644);
        CHECK(dst >= 0, "打开目标文件（越界测试）");
        if (dst < 0) { close(src); goto cleanup_eof; }

        off_t off_in = 100;  /* 超过 EOF */
        off_t off_out = 0;
        CHECK_RET(my_copy_file_range(src, &off_in, dst, &off_out, 1024, 0), 0, "超出 EOF 返回 0");

        close(src);
        close(dst);
    cleanup_eof:
        unlink(TEST_SRC);
        unlink(TEST_DST);
    }

    /* 管道 fd 应返回 EINVAL，copy_file_range 只接受普通文件 */
    {
        int pipefd[2];
        int ret = pipe(pipefd);
        CHECK(ret == 0, "创建管道");
        if (ret != 0) goto cleanup_pipe;

        int dst = open(TEST_DST, O_RDWR | O_CREAT | O_TRUNC, 0644);
        CHECK(dst >= 0, "打开目标文件（管道测试）");
        if (dst < 0) { close(pipefd[0]); close(pipefd[1]); goto cleanup_pipe; }

        CHECK_RET(write(pipefd[1], "pipedata", 8), 8, "写入管道数据");

        off_t off_out = 0;
        ssize_t n = my_copy_file_range(pipefd[0], NULL, dst, &off_out, 8, 0);
        CHECK(n == -1 && errno == EINVAL, "管道作为输入应返回 EINVAL");

        close(pipefd[0]);
        close(pipefd[1]);
        close(dst);
    cleanup_pipe:
        unlink(TEST_DST);
    }

    TEST_DONE();
}
