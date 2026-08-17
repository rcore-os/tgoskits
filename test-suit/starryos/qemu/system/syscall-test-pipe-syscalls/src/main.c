#define _GNU_SOURCE
#include "test_framework.h"
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <pthread.h>
#include <signal.h>
#include <stdint.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>

#define TEST_PIPE_BUF 4096
#define WAIT_STEPS 2000

static volatile sig_atomic_t writer_started;
static volatile sig_atomic_t writer_done;
static volatile sig_atomic_t writer_signal_hits;
static int writer_fd;
static ssize_t writer_result;
static int writer_errno;

static void writer_signal_handler(int signo)
{
    (void)signo;
    writer_signal_hits++;
}

static void *partial_writer(void *arg)
{
    (void)arg;
    char payload[2 * TEST_PIPE_BUF];
    memset(payload, 'p', sizeof(payload));
    writer_started = 1;
    errno = 0;
    writer_result = write(writer_fd, payload, sizeof(payload));
    writer_errno = errno;
    writer_done = 1;
    return NULL;
}

static int wait_for_flag(volatile sig_atomic_t *flag)
{
    for (int step = 0; step < WAIT_STEPS; step++) {
        if (*flag)
            return 1;
        usleep(1000);
    }
    return 0;
}

static int wait_for_pipe_bytes(int fd, int expected)
{
    for (int step = 0; step < WAIT_STEPS; step++) {
        int available = -1;
        if (ioctl(fd, FIONREAD, &available) == 0 && available == expected)
            return 1;
        usleep(1000);
    }
    return 0;
}

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

static void test_pipe_linux_io_semantics(void)
{
    printf("--- pipe Linux I/O semantics ---\n");

    {
        int fds[2];
        CHECK_RET(pipe2(fds, O_NONBLOCK), 0, "zero-length I/O: pipe 创建成功");
        CHECK_RET(syscall(SYS_read, fds[0], NULL, 0), 0,
                  "空 pipe 上 zero-length read 返回 0");
        close(fds[0]);
        struct sigaction ignore = {.sa_handler = SIG_IGN}, old;
        CHECK_RET(sigaction(SIGPIPE, &ignore, &old), 0,
                  "zero-length write: 忽略 SIGPIPE");
        CHECK_RET(syscall(SYS_write, fds[1], NULL, 0), 0,
                  "无 reader 时 zero-length write 仍返回 0");
        CHECK_RET(sigaction(SIGPIPE, &old, NULL), 0,
                  "zero-length write: 恢复 SIGPIPE handler");
        close(fds[1]);
    }

    {
        int fds[2];
        CHECK_RET(pipe2(fds, O_NONBLOCK), 0, "PIPE_BUF 原子性: pipe 创建成功");
        CHECK(fcntl(fds[1], F_SETPIPE_SZ, TEST_PIPE_BUF) == TEST_PIPE_BUF,
              "PIPE_BUF 原子性: capacity 固定为一页");
        char initial[4000];
        char atomic[200];
        memset(initial, 'a', sizeof(initial));
        memset(atomic, 'b', sizeof(atomic));
        CHECK(write(fds[1], initial, sizeof(initial)) == (ssize_t)sizeof(initial),
              "PIPE_BUF 原子性: 先写入 4000 bytes");
        errno = 0;
        CHECK(write(fds[1], atomic, sizeof(atomic)) == -1 && errno == EAGAIN,
              "剩余空间不足时小于 PIPE_BUF 的 nonblocking write 全量 EAGAIN");
        int available = -1;
        CHECK_RET(ioctl(fds[0], FIONREAD, &available), 0,
                  "PIPE_BUF 原子性: FIONREAD 成功");
        CHECK(available == (int)sizeof(initial),
              "失败的原子 write 未提交部分字节");
        close(fds[0]);
        close(fds[1]);
    }

    {
        int fds[2];
        CHECK_RET(pipe(fds), 0, "dup nonblocking: pipe 创建成功");
        int read_dup = dup(fds[0]);
        int write_dup = dup(fds[1]);
        CHECK(read_dup >= 0 && write_dup >= 0, "dup nonblocking: duplicate fd 创建成功");
        CHECK_RET(fcntl(fds[0], F_SETFL, O_NONBLOCK), 0,
                  "dup nonblocking: 在原 read fd 设置 O_NONBLOCK");
        CHECK_RET(fcntl(fds[1], F_SETFL, O_NONBLOCK), 0,
                  "dup nonblocking: 在原 write fd 设置 O_NONBLOCK");
        CHECK((fcntl(read_dup, F_GETFL) & O_NONBLOCK) != 0,
              "dup read fd 共享 O_NONBLOCK 状态");
        CHECK((fcntl(write_dup, F_GETFL) & O_NONBLOCK) != 0,
              "dup write fd 共享 O_NONBLOCK 状态");
        close(read_dup);
        close(write_dup);
        close(fds[0]);
        close(fds[1]);
    }
}

static void test_interrupted_pipe_write_partial_progress(void)
{
    printf("--- interrupted pipe write partial progress ---\n");

    int fds[2];
    CHECK_RET(pipe(fds), 0, "partial write: pipe 创建成功");
    CHECK(fcntl(fds[1], F_SETPIPE_SZ, TEST_PIPE_BUF) == TEST_PIPE_BUF,
          "partial write: capacity 固定为一页");

    char initial[TEST_PIPE_BUF];
    memset(initial, 'i', sizeof(initial));
    CHECK(write(fds[1], initial, sizeof(initial)) == (ssize_t)sizeof(initial),
          "partial write: 预先填满 pipe");

    struct sigaction action;
    struct sigaction old_action;
    memset(&action, 0, sizeof(action));
    action.sa_handler = writer_signal_handler;
    sigemptyset(&action.sa_mask);
    CHECK_RET(sigaction(SIGUSR1, &action, &old_action), 0,
              "partial write: 安装 non-SA_RESTART handler");

    writer_fd = fds[1];
    writer_started = 0;
    writer_done = 0;
    writer_signal_hits = 0;
    writer_result = -1;
    writer_errno = 0;
    pthread_t writer;
    CHECK_RET(pthread_create(&writer, NULL, partial_writer, NULL), 0,
              "partial write: 创建 writer thread");
    CHECK(wait_for_flag(&writer_started), "partial write: writer 已进入 write 路径");

    char consumed[TEST_PIPE_BUF];
    CHECK(read(fds[0], consumed, sizeof(consumed)) == (ssize_t)sizeof(consumed),
          "partial write: reader 释放一页容量");
    int committed = wait_for_pipe_bytes(fds[0], TEST_PIPE_BUF);
    CHECK(committed, "partial write: writer 提交一页后再次阻塞");

    CHECK_RET(pthread_kill(writer, SIGUSR1), 0,
              "partial write: signal 定向中断阻塞 writer");
    int completed_after_signal = wait_for_flag(&writer_done);
    CHECK(completed_after_signal,
          "partial write: 有已提交字节时 signal 使 write 返回而非继续阻塞");
    if (!completed_after_signal)
        close(fds[0]);
    CHECK_RET(pthread_join(writer, NULL), 0, "partial write: 回收 writer thread");
    CHECK(writer_signal_hits >= 1, "partial write: non-SA_RESTART handler 已执行");
    CHECK(writer_result == TEST_PIPE_BUF,
          "partial write: 返回已提交字节数而不是 EINTR/整段重放");
    CHECK(writer_result >= 0 || writer_errno == EINTR,
          "partial write: 失败时 errno 只可能是 EINTR");

    CHECK_RET(sigaction(SIGUSR1, &old_action, NULL), 0,
              "partial write: 恢复 SIGUSR1 handler");
    if (completed_after_signal)
        close(fds[0]);
    close(fds[1]);
}

int main(void)
{
    TEST_START("pipe-syscalls");
    test_pipe();
    test_pipe2();
    test_pipe_linux_io_semantics();
    test_interrupted_pipe_write_partial_progress();
    TEST_DONE();
}
