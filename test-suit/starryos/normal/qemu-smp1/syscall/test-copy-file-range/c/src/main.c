#include "test_framework.h"

#include <unistd.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <sys/stat.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>

#define TEST_SRC  "/tmp/test_cfr_src.tmp"
#define TEST_DST  "/tmp/test_cfr_dst.tmp"
#define TEST_DST2 "/tmp/test_cfr_dst2.tmp"

/* 辅助：创建文件并写入数据，偏移量回到开头 */
static int create_file(const char *path, const char *data, size_t len) {
    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) return -1;
    if (len > 0) {
        write(fd, data, len);
        lseek(fd, 0, SEEK_SET);
    }
    return fd;
}

/* 辅助：读回文件内容并比较 */
static int verify_file_content(const char *path, const char *expected, size_t len) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) return 0;
    char *buf = malloc(len + 1);
    memset(buf, 0, len + 1);
    ssize_t n = read(fd, buf, len);
    close(fd);
    int ok = (n == (ssize_t)len && memcmp(buf, expected, len) == 0);
    free(buf);
    return ok;
}

int main(void) {
    TEST_START("copy_file_range: Copy a range of data from one file to another");

    /* ==================== 正向测试 ==================== */

    /*
     * 测试 1: copy_file_range 基本功能 — 复制整个文件
     * 手册描述: copies up to len bytes of data from fd_in to fd_out.
     *           Upon successful completion, returns the number of bytes copied.
     */
    {
        const char *data = "hello copy_file_range";
        int fd_in = create_file(TEST_SRC, data, strlen(data));
        int fd_out = create_file(TEST_DST, NULL, 0);
        CHECK(fd_in >= 0 && fd_out >= 0, "创建源文件和目标文件成功");

        errno = 0;
        ssize_t n = copy_file_range(fd_in, NULL, fd_out, NULL, strlen(data), 0);
        CHECK(n == (ssize_t)strlen(data),
              "copy_file_range 复制整个文件返回正确字节数");

        close(fd_in);
        close(fd_out);
        CHECK(verify_file_content(TEST_DST, data, strlen(data)),
              "复制后目标文件内容与源文件一致");

        unlink(TEST_SRC);
        unlink(TEST_DST);
    }

    /*
     * 测试 2: off_in/off_out 为 NULL 时文件偏移量自动调整
     * 手册描述: bytes are read from fd_in starting from the file offset,
     *           and the file offset is adjusted by the number of bytes copied.
     */
    {
        const char *data = "ABCDEFGHIJKLMNOP";
        int fd_in = create_file(TEST_SRC, data, strlen(data));
        int fd_out = create_file(TEST_DST, NULL, 0);

        /* 复制前半部分 (8 字节) */
        errno = 0;
        ssize_t n1 = copy_file_range(fd_in, NULL, fd_out, NULL, 8, 0);
        CHECK(n1 == 8, "第一次复制 8 字节成功");

        /* 复制后半部分 (8 字节)，偏移量应自动从 8 开始 */
        errno = 0;
        ssize_t n2 = copy_file_range(fd_in, NULL, fd_out, NULL, 8, 0);
        CHECK(n2 == 8, "第二次复制 8 字节成功（偏移量自动调整）");

        close(fd_in);
        close(fd_out);

        char expected[16];
        memcpy(expected, data, 16);
        CHECK(verify_file_content(TEST_DST, expected, 16),
              "两次复制后目标文件内容完整正确");

        unlink(TEST_SRC);
        unlink(TEST_DST);
    }

    /*
     * 测试 3: 使用 off_in 指定源偏移（不改变文件偏移量）
     * 手册描述: off_in must point to a buffer that specifies the starting offset.
     *           The file offset of fd_in is not changed, but off_in is adjusted.
     */
    {
        const char *data = "ABCDEFGHIJ";
        int fd_in = create_file(TEST_SRC, data, strlen(data));
        /* 读取到末尾使文件偏移量在末尾 */
        lseek(fd_in, 0, SEEK_END);

        int fd_out = create_file(TEST_DST, NULL, 0);
        off_t off_in = 5;  /* 从偏移 5 开始读取 */

        errno = 0;
        ssize_t n = copy_file_range(fd_in, &off_in, fd_out, NULL, 5, 0);
        CHECK(n == 5, "copy_file_range 带指定 off_in=5 读取 5 字节成功");
        CHECK(off_in == 10, "off_in 已调整到 10 (5+5)");

        /* 文件偏移量应不变（仍在末尾） */
        off_t cur = lseek(fd_in, 0, SEEK_CUR);
        CHECK(cur == (off_t)strlen(data),
              "使用 off_in 后文件偏移量未改变");

        close(fd_in);
        close(fd_out);
        CHECK(verify_file_content(TEST_DST, "FGHIJ", 5),
              "off_in=5 复制的内容正确 (FGHIJ)");

        unlink(TEST_SRC);
        unlink(TEST_DST);
    }

    /*
     * 测试 4: 使用 off_out 指定目标偏移
     * 手册描述: Similar statements apply to off_out.
     */
    {
        const char *data = "HELLO";
        int fd_in = create_file(TEST_SRC, data, strlen(data));
        int fd_out = create_file(TEST_DST, "XXXXX", 5);

        off_t off_out = 5;
        errno = 0;
        ssize_t n = copy_file_range(fd_in, NULL, fd_out, &off_out, 5, 0);
        CHECK(n == 5, "copy_file_range 带 off_out=5 写入成功");
        CHECK(off_out == 10, "off_out 已调整到 10 (5+5)");

        close(fd_in);
        close(fd_out);

        char expected[10];
        memcpy(expected, "XXXXX", 5);
        memcpy(expected + 5, "HELLO", 5);
        CHECK(verify_file_content(TEST_DST, expected, 10),
              "off_out=5 后目标文件前 5 字节不变，后 5 字节为复制数据");

        unlink(TEST_SRC);
        unlink(TEST_DST);
    }

    /*
     * 测试 5: 文件偏移量在 EOF 时返回 0
     * 手册描述: If the file offset of fd_in is at or past the end of file,
     *           no bytes are copied, and copy_file_range() returns zero.
     */
    {
        const char *data = "DATA";
        int fd_in = create_file(TEST_SRC, data, strlen(data));
        int fd_out = create_file(TEST_DST, NULL, 0);

        /* 移动到文件末尾之后 */
        lseek(fd_in, 100, SEEK_SET);

        errno = 0;
        ssize_t n = copy_file_range(fd_in, NULL, fd_out, NULL, 10, 0);
        CHECK_RET(n, 0, "copy_file_range fd_in 在 EOF 后返回 0");

        close(fd_in);
        close(fd_out);
        unlink(TEST_SRC);
        unlink(TEST_DST);
    }

    /*
     * 测试 6: 同一文件内复制（不重叠）
     * 手册描述: fd_in and fd_out can refer to the same file.
     */
    {
        const char *data = "ABCDEFGHIJ";
        int fd = create_file(TEST_SRC, data, strlen(data));

        off_t off_in = 0;
        off_t off_out = 5;
        errno = 0;
        ssize_t n = copy_file_range(fd, &off_in, fd, &off_out, 5, 0);
        CHECK(n == 5, "copy_file_range 同一文件不重叠复制成功");

        close(fd);

        /* 验证：偏移 5-9 应变为 ABCDE */
        int fd2 = open(TEST_SRC, O_RDONLY);
        char buf[10] = {0};
        read(fd2, buf, 10);
        close(fd2);
        CHECK(memcmp(buf, "ABCDEABCDE", 10) == 0,
              "同一文件复制后内容正确 (ABCDEABCDE)");

        unlink(TEST_SRC);
    }

    /*
     * 测试 7: 部分复制 — len 大于源文件剩余字节
     * 手册描述: This could be less than the length originally requested.
     */
    {
        const char *data = "SHORT";
        int fd_in = create_file(TEST_SRC, data, strlen(data));
        int fd_out = create_file(TEST_DST, NULL, 0);

        errno = 0;
        ssize_t n = copy_file_range(fd_in, NULL, fd_out, NULL, 1024, 0);
        CHECK(n == (ssize_t)strlen(data),
              "copy_file_range len=1024 但源仅 5 字节，返回 5");

        close(fd_in);
        close(fd_out);
        CHECK(verify_file_content(TEST_DST, data, strlen(data)),
              "部分复制后数据正确");

        unlink(TEST_SRC);
        unlink(TEST_DST);
    }

    /*
     * 测试 8: 循环调用复制大文件
     * 手册描述: copies up to len bytes.
     */
    {
        /* 创建 8192 字节文件 */
        size_t total_size = 8192;
        char *large_data = malloc(total_size);
        for (size_t i = 0; i < total_size; i++)
            large_data[i] = 'A' + (i % 26);

        int fd_in = create_file(TEST_SRC, NULL, 0);
        write(fd_in, large_data, total_size);
        lseek(fd_in, 0, SEEK_SET);

        int fd_out = create_file(TEST_DST, NULL, 0);

        size_t copied = 0;
        while (copied < total_size) {
            errno = 0;
            ssize_t n = copy_file_range(fd_in, NULL, fd_out, NULL,
                                        total_size - copied, 0);
            if (n <= 0) break;
            copied += n;
        }

        CHECK(copied == total_size,
              "copy_file_range 循环复制 8192 字节文件成功");

        close(fd_in);
        close(fd_out);

        /* 验证内容 */
        int fd_v = open(TEST_DST, O_RDONLY);
        char *readback = malloc(total_size);
        read(fd_v, readback, total_size);
        close(fd_v);
        CHECK(memcmp(readback, large_data, total_size) == 0,
              "循环复制后文件内容一致");

        free(large_data);
        free(readback);
        unlink(TEST_SRC);
        unlink(TEST_DST);
    }

    /*
     * 测试 9: 跨进程复制 — 子进程写源文件，父进程复制
     */
    {
        int pipefd[2];
        pipe(pipefd);

        pid_t pid = fork();
        if (pid == 0) {
            close(pipefd[0]);
            int fd = open(TEST_SRC, O_RDWR | O_CREAT | O_TRUNC, 0644);
            write(fd, "child_data", 10);
            close(fd);
            char c = 'R';
            write(pipefd[1], &c, 1);
            close(pipefd[1]);
            _exit(0);
        }

        close(pipefd[1]);
        char c;
        read(pipefd[0], &c, 1);
        close(pipefd[0]);

        int fd_in = open(TEST_SRC, O_RDONLY);
        int fd_out = create_file(TEST_DST, NULL, 0);
        errno = 0;
        ssize_t n = copy_file_range(fd_in, NULL, fd_out, NULL, 10, 0);
        CHECK(n == 10, "copy_file_range 跨进程复制成功");

        close(fd_in);
        close(fd_out);
        CHECK(verify_file_content(TEST_DST, "child_data", 10),
              "跨进程复制后内容正确");

        int status;
        waitpid(pid, &status, 0);
        unlink(TEST_SRC);
        unlink(TEST_DST);
    }

    /*
     * 测试 10: 复制空文件（源文件大小为 0）
     * 手册描述: If the file offset of fd_in is at or past EOF, returns zero.
     */
    {
        int fd_in = create_file(TEST_SRC, NULL, 0);
        int fd_out = create_file(TEST_DST, NULL, 0);

        errno = 0;
        ssize_t n = copy_file_range(fd_in, NULL, fd_out, NULL, 100, 0);
        CHECK_RET(n, 0, "copy_file_range 空源文件返回 0");

        close(fd_in);
        close(fd_out);
        unlink(TEST_SRC);
        unlink(TEST_DST);
    }

    /* ==================== 反向测试 ==================== */

    /*
     * 测试 11: EBADF — 无效的文件描述符
     * 手册描述: One or more file descriptors are not valid.
     */
    {
        int fd = create_file(TEST_DST, NULL, 0);

        CHECK_ERR(copy_file_range(-1, NULL, fd, NULL, 10, 0),
                  EBADF, "copy_file_range fd_in=-1 返回 EBADF");

        CHECK_ERR(copy_file_range(fd, NULL, -1, NULL, 10, 0),
                  EBADF, "copy_file_range fd_out=-1 返回 EBADF");

        CHECK_ERR(copy_file_range(-1, NULL, -1, NULL, 10, 0),
                  EBADF, "copy_file_range 两个 fd 均=-1 返回 EBADF");

        close(fd);
        unlink(TEST_DST);
    }

    /*
     * 测试 12: EBADF — fd_in 不可读
     * 手册描述: fd_in is not open for reading.
     */
    {
        int fd_in = open(TEST_SRC, O_WRONLY | O_CREAT | O_TRUNC, 0644);
        int fd_out = create_file(TEST_DST, NULL, 0);

        CHECK_ERR(copy_file_range(fd_in, NULL, fd_out, NULL, 10, 0),
                  EBADF, "copy_file_range fd_in 不可读 (O_WRONLY) 返回 EBADF");

        close(fd_in);
        close(fd_out);
        unlink(TEST_SRC);
        unlink(TEST_DST);
    }

    /*
     * 测试 13: EBADF — fd_out 不可写
     * 手册描述: fd_out is not open for writing.
     */
    {
        int fd_in = create_file(TEST_SRC, "DATA", 4);
        int fd_out = open(TEST_DST, O_RDONLY | O_CREAT, 0644);

        CHECK_ERR(copy_file_range(fd_in, NULL, fd_out, NULL, 10, 0),
                  EBADF, "copy_file_range fd_out 不可写 (O_RDONLY) 返回 EBADF");

        close(fd_in);
        close(fd_out);
        unlink(TEST_SRC);
        unlink(TEST_DST);
    }

    /*
     * 测试 14: EBADF — fd_out 以 O_APPEND 打开
     * 手册描述: The O_APPEND flag is set for fd_out.
     */
    {
        int fd_in = create_file(TEST_SRC, "DATA", 4);
        int fd_out = open(TEST_DST, O_RDWR | O_CREAT | O_TRUNC | O_APPEND, 0644);

        CHECK_ERR(copy_file_range(fd_in, NULL, fd_out, NULL, 10, 0),
                  EBADF, "copy_file_range fd_out O_APPEND 返回 EBADF");

        close(fd_in);
        close(fd_out);
        unlink(TEST_SRC);
        unlink(TEST_DST);
    }

    /*
     * 测试 15: EINVAL — flags 不为 0
     * 手册描述: The flags argument is not 0.
     */
    {
        int fd_in = create_file(TEST_SRC, "DATA", 4);
        int fd_out = create_file(TEST_DST, NULL, 0);

        CHECK_ERR(copy_file_range(fd_in, NULL, fd_out, NULL, 10, 1),
                  EINVAL, "copy_file_range flags=1 返回 EINVAL");

        close(fd_in);
        close(fd_out);
        unlink(TEST_SRC);
        unlink(TEST_DST);
    }

    /*
     * 测试 16: EINVAL — 同一文件且范围重叠
     * 手册描述: fd_in and fd_out refer to the same file and the source
     *           and target ranges overlap.
     */
    {
        const char *data = "ABCDEFGHIJ";
        int fd = create_file(TEST_SRC, data, strlen(data));

        /* off_in=0, off_out=3, len=5 → [0..4] 和 [3..7] 重叠 */
        off_t off_in = 0;
        off_t off_out = 3;
        CHECK_ERR(copy_file_range(fd, &off_in, fd, &off_out, 5, 0),
                  EINVAL, "copy_file_range 同一文件范围重叠返回 EINVAL");

        close(fd);
        unlink(TEST_SRC);
    }

    /*
     * 测试 17: EINVAL — fd 之一不是常规文件（使用管道）
     * 手册描述: Either fd_in or fd_out is not a regular file.
     */
    {
        int pipefd[2];
        pipe(pipefd);
        int fd = create_file(TEST_DST, NULL, 0);

        CHECK_ERR(copy_file_range(pipefd[0], NULL, fd, NULL, 10, 0),
                  EINVAL, "copy_file_range fd_in=管道 返回 EINVAL");

        CHECK_ERR(copy_file_range(fd, NULL, pipefd[1], NULL, 10, 0),
                  EINVAL, "copy_file_range fd_out=管道 返回 EINVAL");

        close(pipefd[0]);
        close(pipefd[1]);
        close(fd);
        unlink(TEST_DST);
    }

    /*
     * 测试 18: EISDIR — fd 指向目录
     * 手册描述: Either fd_in or fd_out refers to a directory.
     */
    {
        int dir_fd = open("/tmp", O_RDONLY);
        int fd_out = create_file(TEST_DST, NULL, 0);
        int fd_in = create_file(TEST_SRC, "X", 1);

        CHECK_ERR(copy_file_range(dir_fd, NULL, fd_out, NULL, 10, 0),
                  EISDIR, "copy_file_range fd_in=目录 返回 EISDIR");

        CHECK_ERR(copy_file_range(fd_in, NULL, dir_fd, NULL, 10, 0),
                  EISDIR, "copy_file_range fd_out=目录 返回 EISDIR");

        close(dir_fd);
        close(fd_in);
        close(fd_out);
        unlink(TEST_SRC);
        unlink(TEST_DST);
    }

    /*
     * 测试 19: EBADF — 已关闭的 fd
     */
    {
        int fd_in = create_file(TEST_SRC, "DATA", 4);
        int fd_out = create_file(TEST_DST, NULL, 0);
        close(fd_in);

        CHECK_ERR(copy_file_range(fd_in, NULL, fd_out, NULL, 10, 0),
                  EBADF, "copy_file_range 已关闭的 fd_in 返回 EBADF");

        close(fd_out);
        unlink(TEST_SRC);
        unlink(TEST_DST);
    }

    /*
     * 测试 20: EINVAL — len 为 0
     * 边界条件：len=0
     */
    {
        int fd_in = create_file(TEST_SRC, "DATA", 4);
        int fd_out = create_file(TEST_DST, NULL, 0);

        errno = 0;
        ssize_t n = copy_file_range(fd_in, NULL, fd_out, NULL, 0, 0);
        /* len=0 可返回 0 或 EINVAL，取决于内核版本 */
        CHECK(n == 0 || (n == -1 && errno == EINVAL),
              "copy_file_range len=0 返回 0 或 EINVAL (均为合理行为)");

        close(fd_in);
        close(fd_out);
        unlink(TEST_SRC);
        unlink(TEST_DST);
    }

    /* ==================== 清理 ==================== */
    unlink(TEST_SRC);
    unlink(TEST_DST);
    unlink(TEST_DST2);

    TEST_DONE();
}