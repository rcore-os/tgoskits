#define _GNU_SOURCE
#include "test_framework.h"
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <string.h>
#include <signal.h>
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
        pipe(fds);
        close(fds[1]);
        char buf[8];
        ssize_t r = read(fds[0], buf, sizeof(buf));
        CHECK(r == 0, "关闭写端后 read 返回 0 (EOF)");
        close(fds[0]);
    }

    {
        int fds[2];
        pipe(fds);
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
        pipe(fds);
        CHECK(get_cloexec(fds[0]) == 0, "pipe fd[0] 默认非 CLOEXEC");
        CHECK(get_cloexec(fds[1]) == 0, "pipe fd[1] 默认非 CLOEXEC");
        close(fds[0]);
        close(fds[1]);
    }

    {
        int fds[2];
        pipe(fds);
        const char *msg = "leftover";
        write(fds[1], msg, strlen(msg));
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
        write(fds[1], msg, strlen(msg));
        char buf[16] = {0};
        read(fds[0], buf, sizeof(buf) - 1);
        CHECK(strcmp(buf, msg) == 0, "pipe2 flags=0 读写正确");
        close(fds[0]);
        close(fds[1]);
    }

    {
        int fds[2];
        CHECK_RET(pipe2(fds, 0), 0, "pipe2 fd 排序准备");
        CHECK(fds[0] < fds[1], "pipe2 fd[0] < fd[1]");
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
        pipe2(fds, O_NONBLOCK);
        int count = 0;
        char buf[4096];
        memset(buf, 'x', sizeof(buf));
        while (write(fds[1], buf, sizeof(buf)) > 0) {
            count++;
            if (count > 10000) break;
        }
        CHECK(errno == EAGAIN || errno == EWOULDBLOCK,
              "O_NONBLOCK 写满 pipe 返回 EAGAIN/EWOULDBLOCK");
        close(fds[0]);
        close(fds[1]);
    }

    {
        int *bad_ptr = (int *)(uintptr_t)0x1;
        CHECK_ERR(pipe2(bad_ptr, 0), EFAULT, "pipe2 无效 fds 指针 -> EFAULT");
    }
}

int main(void)
{
    TEST_START("pipe-syscalls");
    test_pipe();
    test_pipe2();
    TEST_DONE();
}
