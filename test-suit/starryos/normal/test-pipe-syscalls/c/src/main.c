#define _GNU_SOURCE
#include "test_framework.h"
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <string.h>
#include <signal.h>
#include <sys/syscall.h>
#include <stdint.h>

static int get_cloexec(int fd)
{
    int flags = fcntl(fd, F_GETFD);
    if (flags == -1) return -1;
    return !!(flags & FD_CLOEXEC);
}

static void test_pipe(void)
{
    printf("--- pipe ---\n");

    {
        int fds[2];
        CHECK_RET(pipe(fds), 0, "pipe 创建成功");
        CHECK(fds[0] >= 0, "pipe fd[0] >= 0");
        CHECK(fds[1] >= 0, "pipe fd[1] >= 0");
        CHECK(fds[0] != fds[1], "pipe fd[0] != fd[1]");
        const char *msg = "hello pipe";
        ssize_t wlen = write(fds[1], msg, strlen(msg));
        CHECK(wlen == (ssize_t)strlen(msg), "pipe write 数据完整");
        char buf[64] = {0};
        ssize_t rlen = read(fds[0], buf, sizeof(buf) - 1);
        CHECK(rlen == (ssize_t)strlen(msg), "pipe read 数据完整");
        CHECK(strcmp(buf, msg) == 0, "pipe read 内容正确");
        close(fds[0]);
        close(fds[1]);
    }

    {
        int fds[2];
        CHECK_RET(pipe(fds), 0, "EOF 测试: pipe 创建成功");
        close(fds[1]);
        char buf[8];
        ssize_t r = read(fds[0], buf, sizeof(buf));
        CHECK(r == 0, "关闭写端后 read 返回 0 (EOF)");
        close(fds[0]);
    }

    {
        int fds[2];
        CHECK_RET(pipe(fds), 0, "EPIPE 测试: pipe 创建成功");
        close(fds[0]);
        struct sigaction sa = {.sa_handler = SIG_IGN}, old;
        sigaction(SIGPIPE, &sa, &old);
        ssize_t r = write(fds[1], "x", 1);
        CHECK(r == -1 && errno == EPIPE, "关闭读端后 write 返回 EPIPE");
        sigaction(SIGPIPE, &old, NULL);
        close(fds[1]);
    }

    {
        int fds[2];
        CHECK_RET(pipe(fds), 0, "CLOEXEC 默认值测试: pipe 创建成功");
        CHECK(get_cloexec(fds[0]) == 0, "pipe fd[0] 默认非 CLOEXEC");
        CHECK(get_cloexec(fds[1]) == 0, "pipe fd[1] 默认非 CLOEXEC");
        close(fds[0]);
        close(fds[1]);
    }

    {
        int fds[2];
        CHECK_RET(pipe(fds), 0, "残留数据测试: pipe 创建成功");
        const char *msg = "leftover";
        ssize_t wlen = write(fds[1], msg, strlen(msg));
        CHECK(wlen == (ssize_t)strlen(msg), "残留数据写入完整");
        close(fds[1]);
        char buf[64] = {0};
        ssize_t r1 = read(fds[0], buf, sizeof(buf) - 1);
        CHECK(r1 == (ssize_t)strlen(msg), "关闭写端后读取残留数据完整");
        CHECK(strcmp(buf, msg) == 0, "残留数据内容正确");
        ssize_t r2 = read(fds[0], buf, sizeof(buf));
        CHECK(r2 == 0, "残留数据读完后再次 read 返回 0 (EOF)");
        close(fds[0]);
    }
}

static void test_pipe2(void)
{
    printf("--- pipe2 ---\n");

    {
        int fds[2];
        CHECK_RET(pipe2(fds, 0), 0, "pipe2 flags=0 成功");
        CHECK(fds[0] >= 0 && fds[1] >= 0, "pipe2 flags=0 fd 有效");
        const char *msg = "pipe2";
        ssize_t wlen = write(fds[1], msg, strlen(msg));
        CHECK(wlen == (ssize_t)strlen(msg), "pipe2 flags=0 写入完整");
        char buf[16] = {0};
        ssize_t rlen = read(fds[0], buf, sizeof(buf) - 1);
        CHECK(rlen == (ssize_t)strlen(msg), "pipe2 flags=0 读取完整");
        CHECK(strcmp(buf, msg) == 0, "pipe2 flags=0 读写正确");
        close(fds[0]);
        close(fds[1]);
    }

    /* POSIX only guarantees fds[0] is the read end and fds[1] is the
     * write end — not that their numeric values are ordered.
     * Verify the role semantics instead. */
    {
        int fds[2];
        CHECK_RET(pipe2(fds, 0), 0, "pipe2 读写端语义准备");
        CHECK(fds[0] != fds[1], "pipe2 两端 fd 不同");
        errno = 0;
        ssize_t wr = write(fds[0], "x", 1);
        CHECK(wr == -1, "fd[0] 是只读端，write 失败");
        errno = 0;
        char tmp[8];
        ssize_t rd = read(fds[1], tmp, sizeof(tmp));
        CHECK(rd == -1, "fd[1] 是只写端，read 失败");
        close(fds[0]);
        close(fds[1]);
    }

    {
        int fds[2];
        CHECK_RET(pipe2(fds, O_NONBLOCK), 0, "pipe2 O_NONBLOCK 成功");
        char buf[8];
        errno = 0;
        ssize_t r = read(fds[0], buf, sizeof(buf));
        CHECK(r == -1 && errno == EAGAIN, "O_NONBLOCK 读空 pipe 返回 EAGAIN");
        close(fds[0]);
        close(fds[1]);
    }

    {
        int fds[2];
        CHECK_RET(pipe2(fds, O_CLOEXEC), 0, "pipe2 O_CLOEXEC 成功");
        CHECK(get_cloexec(fds[0]) == 1, "pipe2 O_CLOEXEC fd[0] 有 CLOEXEC");
        CHECK(get_cloexec(fds[1]) == 1, "pipe2 O_CLOEXEC fd[1] 有 CLOEXEC");
        close(fds[0]);
        close(fds[1]);
    }

    {
        int fds[2];
        CHECK_RET(pipe2(fds, O_NONBLOCK | O_CLOEXEC), 0, "pipe2 O_NONBLOCK|O_CLOEXEC 成功");
        CHECK(get_cloexec(fds[0]) == 1, "组合标志 fd[0] CLOEXEC");
        char buf[8];
        errno = 0;
        ssize_t r = read(fds[0], buf, sizeof(buf));
        CHECK(r == -1 && errno == EAGAIN, "组合标志 读空返回 EAGAIN");
        close(fds[0]);
        close(fds[1]);
    }

    {
        int fds[2];
        CHECK_RET(pipe2(fds, O_NONBLOCK), 0, "pipe2 写满测试准备");
        int count = 0;
        char buf[4096];
        memset(buf, 'x', sizeof(buf));
        ssize_t w;
        while ((w = write(fds[1], buf, sizeof(buf))) > 0) {
            count++;
            if (count > 10000) break;
        }
        CHECK(w == -1 && (errno == EAGAIN || errno == EWOULDBLOCK),
              "O_NONBLOCK 写满 pipe 返回 EAGAIN/EWOULDBLOCK");
        close(fds[0]);
        close(fds[1]);
    }

    {
        /* Use direct syscall: glibc's pipe2 wrapper triggers
         * -Werror=stringop-overflow when passed an invalid pointer.
         * Directly invoking SYS_pipe2 tests the kernel's copy_to_user
         * error path without triggering static analysis diagnostics. */
        CHECK_ERR(syscall(SYS_pipe2, (int *)(uintptr_t)0x1, 0), EFAULT,
                  "SYS_pipe2 无效 fds 指针 -> EFAULT");
    }
}

int main(void)
{
    TEST_START("pipe-syscalls");
    test_pipe();
    test_pipe2();
    TEST_DONE();
}
