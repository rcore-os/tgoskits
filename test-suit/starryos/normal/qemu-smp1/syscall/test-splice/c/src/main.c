/*
 * test_splice.c — 验证 splice 系统调用的参数校验及基本功能。
 *
 * 覆盖场景：
 *   1. regular file -> regular file 返回 EINVAL
 *   2. file -> pipe 数据传输，验证 off_in 更新
 *   3. pipe -> file 数据传输，验证 off_out 更新
 *   4. pipe -> pipe 数据传输
 *   5. fd_in 是 pipe 且 off_in 非 NULL 返回 ESPIPE
 *   6. fd_out 是 pipe 且 off_out 非 NULL 返回 ESPIPE
 *   7. unknown flags 返回 EINVAL
 *   8. EOF 后 splice 返回 0，off_in 不变
 *   9. len=0 返回 0，off_in 不变
 *   10. bad input fd 返回 EBADF
 *   11. bad output fd 返回 EBADF
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

#ifndef SPLICE_F_MOVE
#define SPLICE_F_MOVE 1
#endif

static ssize_t my_splice(int fd_in, off_t *off_in,
                         int fd_out, off_t *off_out,
                         size_t len, unsigned int flags)
{
    return syscall(SYS_splice, fd_in, off_in, fd_out, off_out, len, flags);
}

#define TEST_SRC "/tmp/starry_test_splice_src"
#define TEST_DST "/tmp/starry_test_splice_dst"

static int reset_file(const char *path, const char *data)
{
    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    CHECK(fd >= 0, "open reset file");
    if (fd < 0) {
        return -1;
    }

    size_t len = strlen(data);
    CHECK_RET(write(fd, data, len), (ssize_t)len, "write all data");

    return fd;
}

static int read_file(const char *path, char *buf, size_t size)
{
    int fd = open(path, O_RDONLY);
    CHECK(fd >= 0, "open file for read");
    if (fd < 0) {
        return -1;
    }

    ssize_t n = read(fd, buf, size);
    CHECK(n >= 0, "read file content");

    close(fd);
    return (int)n;
}

int main(void)
{
    TEST_START("splice");

    /* regular file -> regular file must fail with EINVAL */
    {
        int src = reset_file(TEST_SRC, "abcdef");
        int dst = open(TEST_DST, O_RDWR | O_CREAT | O_TRUNC, 0644);
        CHECK(dst >= 0, "open destination file");

        if (src >= 0 && dst >= 0) {
            ssize_t n = my_splice(src, NULL, dst, NULL, 3, 0);
            CHECK(n == -1, "regular file to regular file should fail");
            CHECK(errno == EINVAL, "regular file to regular file should return EINVAL");
        }

        if (src >= 0) close(src);
        if (dst >= 0) close(dst);
        unlink(TEST_SRC);
        unlink(TEST_DST);
    }

    /* file -> pipe with off_in */
    {
        int src = reset_file(TEST_SRC, "abcdef");
        int pipefd[2];
        CHECK(src >= 0, "open source file");
        CHECK(pipe(pipefd) == 0, "create pipe");

        if (src >= 0 && pipefd[0] >= 0 && pipefd[1] >= 0) {
            off_t off_in = 1;
            ssize_t n = my_splice(src, &off_in, pipefd[1], NULL, 3, 0);
            CHECK(n == 3, "file to pipe should splice 3 bytes");
            CHECK(off_in == 4, "off_in should be updated");

            off_t cur = lseek(src, 0, SEEK_CUR);
            CHECK(cur == 0, "fd_in offset should not change when off_in is not NULL");

            char buf[8] = {0};
            CHECK_RET(read(pipefd[0], buf, 3), 3, "read 3 bytes from pipe");
            CHECK(memcmp(buf, "bcd", 3) == 0, "pipe content should be bcd");
        }

        if (src >= 0) close(src);
        close(pipefd[0]);
        close(pipefd[1]);
        unlink(TEST_SRC);
    }

    /* pipe -> file with off_out */
    {
        int dst = open(TEST_DST, O_RDWR | O_CREAT | O_TRUNC, 0644);
        CHECK(dst >= 0, "open destination file");

        int pipefd[2];
        CHECK(pipe(pipefd) == 0, "create pipe");

        CHECK_RET(write(pipefd[1], "abc", 3), 3, "write all data");

        if (dst >= 0) {
            off_t off_out = 0;
            ssize_t n = my_splice(pipefd[0], NULL, dst, &off_out, 3, 0);
            CHECK(n == 3, "pipe to file should splice 3 bytes");
            CHECK(off_out == 3, "off_out should be updated");

            char buf[8] = {0};
            int r = read_file(TEST_DST, buf, sizeof(buf));
            CHECK(r >= 3 && memcmp(buf, "abc", 3) == 0, "destination file content should be abc");
        }

        close(pipefd[0]);
        close(pipefd[1]);
        if (dst >= 0) close(dst);
        unlink(TEST_DST);
    }

    /* pipe -> pipe */
    {
        int in_pipe[2];
        int out_pipe[2];

        CHECK(pipe(in_pipe) == 0, "create input pipe");
        CHECK(pipe(out_pipe) == 0, "create output pipe");

        CHECK_RET(write(in_pipe[1], "xyz", 3), 3, "write all data");

        ssize_t n = my_splice(in_pipe[0], NULL, out_pipe[1], NULL, 3, 0);
        CHECK(n == 3, "pipe to pipe should splice 3 bytes");

        char buf[8] = {0};
        CHECK_RET(read(out_pipe[0], buf, 3), 3, "read 3 bytes from output pipe");
        CHECK(memcmp(buf, "xyz", 3) == 0, "output pipe content should be xyz");

        close(in_pipe[0]);
        close(in_pipe[1]);
        close(out_pipe[0]);
        close(out_pipe[1]);
    }

    /* fd_in is pipe and off_in is not NULL -> ESPIPE */
    {
        int dst = open(TEST_DST, O_RDWR | O_CREAT | O_TRUNC, 0644);
        CHECK(dst >= 0, "open destination file");

        int pipefd[2];
        CHECK(pipe(pipefd) == 0, "create pipe");
        CHECK_RET(write(pipefd[1], "abc", 3), 3, "write all data");

        off_t off_in = 0;
        ssize_t n = my_splice(pipefd[0], &off_in, dst, NULL, 3, 0);
        CHECK(n == -1, "fd_in is pipe and off_in is not NULL should fail");
        CHECK(errno == ESPIPE, "fd_in is pipe and off_in is not NULL should return ESPIPE");

        close(pipefd[0]);
        close(pipefd[1]);
        if (dst >= 0) close(dst);
        unlink(TEST_DST);
    }

    /* fd_out is pipe and off_out is not NULL -> ESPIPE */
    {
        int src = reset_file(TEST_SRC, "abcdef");
        CHECK(src >= 0, "open source file");

        int pipefd[2];
        CHECK(pipe(pipefd) == 0, "create pipe");

        off_t off_out = 0;
        ssize_t n = my_splice(src, NULL, pipefd[1], &off_out, 3, 0);
        CHECK(n == -1, "fd_out is pipe and off_out is not NULL should fail");
        CHECK(errno == ESPIPE, "fd_out is pipe and off_out is not NULL should return ESPIPE");

        if (src >= 0) close(src);
        close(pipefd[0]);
        close(pipefd[1]);
        unlink(TEST_SRC);
    }

    /* unknown flags -> EINVAL */
    {
        int src = reset_file(TEST_SRC, "abcdef");
        CHECK(src >= 0, "open source file");

        int pipefd[2];
        CHECK(pipe(pipefd) == 0, "create pipe");

        ssize_t n = my_splice(src, NULL, pipefd[1], NULL, 3, 0x80000000u);
        CHECK(n == -1, "unknown flags should fail");
        CHECK(errno == EINVAL, "unknown flags should return EINVAL");

        if (src >= 0) close(src);
        close(pipefd[0]);
        close(pipefd[1]);
        unlink(TEST_SRC);
    }

    /* EOF -> 0 and off_in unchanged */
    {
        int src = reset_file(TEST_SRC, "abc");
        CHECK(src >= 0, "open source file");

        int pipefd[2];
        CHECK(pipe(pipefd) == 0, "create pipe");

        off_t off_in = 10;
        ssize_t n = my_splice(src, &off_in, pipefd[1], NULL, 3, 0);
        CHECK(n == 0, "splice after EOF should return 0");
        CHECK(off_in == 10, "off_in should not change after EOF");

        if (src >= 0) close(src);
        close(pipefd[0]);
        close(pipefd[1]);
        unlink(TEST_SRC);
    }

    /* len=0 -> 0 and off_in unchanged */
    {
        int src = reset_file(TEST_SRC, "abc");
        CHECK(src >= 0, "open source file");

        int pipefd[2];
        CHECK(pipe(pipefd) == 0, "create pipe");

        off_t off_in = 1;
        ssize_t n = my_splice(src, &off_in, pipefd[1], NULL, 0, 0);
        CHECK(n == 0, "count zero should return 0");
        CHECK(off_in == 1, "off_in should not change when count is zero");

        if (src >= 0) close(src);
        close(pipefd[0]);
        close(pipefd[1]);
        unlink(TEST_SRC);
    }

    {
        int pipefd[2];
        CHECK(pipe(pipefd) == 0, "create pipe");

        ssize_t n = my_splice(-1, NULL, pipefd[1], NULL, 3, 0);
        CHECK(n == -1, "bad input fd should fail");
        CHECK(errno == EBADF, "bad input fd should return EBADF");

        close(pipefd[0]);
        close(pipefd[1]);
    }

    /* bad output fd -> EBADF */
    {
        int src = reset_file(TEST_SRC, "abc");
        CHECK(src >= 0, "open source file");

        ssize_t n = my_splice(src, NULL, -1, NULL, 3, 0);
        CHECK(n == -1, "bad output fd should fail");
        CHECK(errno == EBADF, "bad output fd should return EBADF");

        if (src >= 0) close(src);
        unlink(TEST_SRC);
    }

      {
        int dst = open(TEST_DST, O_RDWR | O_CREAT | O_TRUNC, 0644);
        CHECK(dst >= 0, "open destination file for pipe direction test");

        int pipefd[2];
        CHECK(pipe(pipefd) == 0, "create pipe for direction test");

        ssize_t n = my_splice(pipefd[1], NULL, dst, NULL, 3, 0);
        CHECK(n == -1, "pipe write end used as input should fail");
        CHECK(errno == EBADF, "pipe write end used as input should return EBADF");

        int src = reset_file(TEST_SRC, "abc");
        CHECK(src >= 0, "open source file for pipe direction test");

        n = my_splice(src, NULL, pipefd[0], NULL, 3, 0);
        CHECK(n == -1, "pipe read end used as output should fail");
        CHECK(errno == EBADF, "pipe read end used as output should return EBADF");

        if (src >= 0) close(src);
        if (dst >= 0) close(dst);
        close(pipefd[0]);
        close(pipefd[1]);
        unlink(TEST_SRC);
        unlink(TEST_DST);
    }
     /* same pipe used as both input and output -> EINVAL */
    {
        int pipefd[2];
        CHECK(pipe(pipefd) == 0, "create pipe for same-pipe test");

        CHECK_RET(write(pipefd[1], "abc", 3), 3, "write data for same-pipe test");

        ssize_t n = my_splice(pipefd[0], NULL, pipefd[1], NULL, 3, 0);
        CHECK(n == -1, "same pipe input and output should fail");
        CHECK(errno == EINVAL, "same pipe input and output should return EINVAL");

        close(pipefd[0]);
        close(pipefd[1]);
    }
        /* O_APPEND output -> EINVAL */
    {
        int pipefd[2];
        CHECK(pipe(pipefd) == 0, "create pipe for O_APPEND test");

        CHECK_RET(write(pipefd[1], "abc", 3), 3, "write data for O_APPEND test");

        int dst = open(TEST_DST, O_RDWR | O_CREAT | O_TRUNC | O_APPEND, 0644);
        CHECK(dst >= 0, "open O_APPEND destination file");

        ssize_t n = my_splice(pipefd[0], NULL, dst, NULL, 3, 0);
        CHECK(n == -1, "O_APPEND output should fail");
        CHECK(errno == EINVAL, "O_APPEND output should return EINVAL");

        close(pipefd[0]);
        close(pipefd[1]);
        if (dst >= 0) close(dst);
        unlink(TEST_DST);
    }

    TEST_DONE();
}
