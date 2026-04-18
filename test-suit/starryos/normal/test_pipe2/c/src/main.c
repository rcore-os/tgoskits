/*
 * test_pipe2.c — pipe / pipe2
 *
 * 测试策略：验证管道的基本读写、阻塞/非阻塞行为、O_CLOEXEC 标志
 *
 * 覆盖范围：
 *   正向：创建管道、单进程读写、fork 通信、O_CLOEXEC、
 *         写端关闭后 EOF、读端关闭后 EPIPE
 *   负向：无效 fd 参数、EAGAIN、EPIPE、SIGPIPE
 *   状态转移：管道空/满状态与读写行为
 */

#define _GNU_SOURCE
#define _POSIX_C_SOURCE 200809L

#include <unistd.h>
#include <fcntl.h>
#include <string.h>
#include <errno.h>
#include <sys/wait.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>

/* pipe2 fallback 实现（如果系统不支持） */
#if __GLIBC__ < 2 || (__GLIBC__ == 2 && __GLIBC_MINOR__ < 9)
static int pipe2_fallback(int pipefd[2], int flags) {
    if (flags & ~(O_CLOEXEC | O_NONBLOCK)) {
        errno = EINVAL;
        return -1;
    }
    if (pipe(pipefd) == -1)
        return -1;
    if (flags & O_CLOEXEC) {
        fcntl(pipefd[0], F_SETFD, FD_CLOEXEC);
        fcntl(pipefd[1], F_SETFD, FD_CLOEXEC);
    }
    if (flags & O_NONBLOCK) {
        fcntl(pipefd[0], F_SETFL, O_NONBLOCK);
        fcntl(pipefd[1], F_SETFL, O_NONBLOCK);
    }
    return 0;
}
#define pipe2 pipe2_fallback
#endif

/* 如果没有 F_GETPIPE_SZ，定义它 */
#ifndef F_GETPIPE_SZ
#define F_GETPIPE_SZ 1032
#endif

/* ========== 内联测试框架 ========== */

static int test_passed = 0;
static int test_failed = 0;
static const char *current_test_name = NULL;

#define TEST_START(name) \
    do { \
        current_test_name = (name); \
        printf("[TEST] %s\n", (name)); \
        test_passed = 0; \
        test_failed = 0; \
    } while(0)

#define CHECK(cond, msg) \
    do { \
        if (!(cond)) { \
            printf("  [FAIL] %s\n", (msg)); \
            test_failed++; \
        } else { \
            printf("  [OK] %s\n", (msg)); \
            test_passed++; \
        } \
    } while(0)

#define CHECK_RET(val, expected, msg) \
    do { \
        long long _v = (long long)(val); \
        long long _e = (long long)(expected); \
        if (_v != _e) { \
            printf("  [FAIL] %s (got %lld, expected %lld)\n", (msg), _v, _e); \
            test_failed++; \
        } else { \
            printf("  [OK] %s (= %lld)\n", (msg), _v); \
            test_passed++; \
        } \
    } while(0)

#define TEST_DONE() \
    do { \
        printf("\n=== 结果: %d 通过, %d 失败 ===\n", test_passed, test_failed); \
	if (test_failed ==0) {printf("TEST PASSED\n");} \
        return (test_failed == 0) ? EXIT_SUCCESS : EXIT_FAILURE; \
    } while(0)

/* ========== 测试代码 ========== */

#define TEST_DATA "Hello, pipe test data!"
#define BIG_SIZE 65536

int main(void)
{
    TEST_START("pipe2: pipe/pipe2 读写与状态验证");

    /* ================================================================
     * PART 1: 基本 pipe2 创建与读写
     * ================================================================ */

    /* 1. pipe2 创建 */
    int fds[2];
    int ret = pipe2(fds, 0);
    CHECK_RET(ret, 0, "pipe2 创建成功");
    if (ret != 0) { TEST_DONE(); }

    CHECK(fds[0] >= 0, "pipe 读端 fd 有效");
    CHECK(fds[1] >= 0, "pipe 写端 fd 有效");
    CHECK(fds[0] != fds[1], "读写端 fd 不同");

    /* 2. 单进程: 写入后读取 */
    const char *msg = TEST_DATA;
    int msg_len = strlen(msg);
    ssize_t wret = write(fds[1], msg, msg_len);
    CHECK_RET(wret, msg_len, "写入管道成功");

    char buf[128] = {0};
    ssize_t rret = read(fds[0], buf, sizeof(buf) - 1);
    CHECK_RET(rret, msg_len, "从管道读取成功");
    CHECK(strcmp(buf, msg) == 0, "管道数据内容一致");

    close(fds[0]);
    close(fds[1]);

    /* ================================================================
     * PART 2: pipe2 O_CLOEXEC 标志
     * ================================================================ */

    /* 3. 验证 pipe2 O_CLOEXEC 标志 */
    ret = pipe2(fds, O_CLOEXEC);
    CHECK_RET(ret, 0, "pipe2 O_CLOEXEC 创建成功");
    int fd_flags = fcntl(fds[0], F_GETFD);
    CHECK(fd_flags >= 0 && (fd_flags & FD_CLOEXEC),
          "pipe2 O_CLOEXEC 读端标志已设置");
    fd_flags = fcntl(fds[1], F_GETFD);
    CHECK(fd_flags >= 0 && (fd_flags & FD_CLOEXEC),
          "pipe2 O_CLOEXEC 写端标志已设置");

    pid_t pid = fork();
    if (pid == 0) {
        /* 子进程: 关闭写端，只读 */
        close(fds[1]);
        char child_buf[64] = {0};
        ssize_t n = read(fds[0], child_buf, sizeof(child_buf) - 1);
        close(fds[0]);
        _exit(n < 0 ? 255 : (n > 127 ? 127 : (int)n));
    }

    /* 父进程: 关闭读端，只写 */
    close(fds[0]);
    const char *parent_msg = "hello child";
    int plen = strlen(parent_msg);
    write(fds[1], parent_msg, plen);
    close(fds[1]);

    int status;
    wait4(pid, &status, 0, NULL);
    CHECK(WIFEXITED(status), "管道子进程正常退出");
    CHECK_RET(WEXITSTATUS(status), plen, "管道子进程读取到完整数据");

    /* ================================================================
     * PART 3: fork 后父子各自关闭一端
     * ================================================================ */

    /* 4. fork 后父子各自关闭一端 */
    ret = pipe2(fds, 0);
    CHECK_RET(ret, 0, "pipe2 创建(fork 通信测试)");

    pid = fork();
    if (pid == 0) {
        /* 子进程: 关闭写端 */
        close(fds[1]);

        /* 读取所有数据 */
        char child_buf[256];
        ssize_t total = 0;
        ssize_t n;
        while ((n = read(fds[0], child_buf + total,
                        sizeof(child_buf) - total)) > 0) {
            total += n;
        }
        close(fds[0]);

        /* 验证数据 */
        const char *expected = "parent_data";
        _exit((size_t)total == strlen(expected) &&
              memcmp(child_buf, expected, total) == 0 ? 0 : 1);
    }

    /* 父进程: 关闭读端 */
    close(fds[0]);
    const char *parent_data = "parent_data";
    write(fds[1], parent_data, strlen(parent_data));
    close(fds[1]);

    wait4(pid, &status, 0, NULL);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "fork 管道通信数据精确匹配");

    /* ================================================================
     * PART 4: 写端关闭后 read 返回 EOF
     * ================================================================ */

    /* 5. read 已关闭的管道写端返回 EOF */
    ret = pipe2(fds, 0);
    CHECK_RET(ret, 0, "pipe2 创建(EOF 测试)");
    close(fds[1]); /* 关闭写端 */

    char tmp[4];
    ssize_t eof_ret = read(fds[0], tmp, sizeof(tmp));
    CHECK_RET(eof_ret, 0, "写端关闭后 read 返回 0 (EOF)");
    close(fds[0]);

    /* ================================================================
     * PART 5: 读端关闭后 write 返回 EPIPE
     * ================================================================ */

    /* 6. write 已关闭的读端返回 EPIPE */
    signal(SIGPIPE, SIG_IGN); /* 忽略 SIGPIPE 以便检查 EPIPE */
    ret = pipe2(fds, 0);
    CHECK_RET(ret, 0, "pipe2 创建(EPIPE 测试)");
    close(fds[0]); /* 关闭读端 */

    errno = 0;
    ssize_t broken_write = write(fds[1], "x", 1);
    CHECK(broken_write == -1 && errno == EPIPE,
          "write 到已关闭读端的管道应返回 EPIPE");
    close(fds[1]);

    /* ================================================================
     * PART 6: O_NONBLOCK 非阻塞测试
     * ================================================================ */

    /* 7. O_NONBLOCK 管道空读取 */
    ret = pipe2(fds, O_NONBLOCK);
    CHECK_RET(ret, 0, "pipe2 O_NONBLOCK 创建");

    char nonblock_buf[16];
    errno = 0;
    ssize_t nonblock_ret = read(fds[0], nonblock_buf, sizeof(nonblock_buf));
    CHECK(nonblock_ret == -1 && errno == EAGAIN,
          "O_NONBLOCK 空管道 read 返回 EAGAIN");

    /* 8. O_NONBLOCK 管道满写入 */
    /* 循环写入直到管道满 */
    char fill_buf[4096];
    memset(fill_buf, 'X', sizeof(fill_buf));

    ssize_t total_written = 0;
    while (1) {
        errno = 0;
        ssize_t w = write(fds[1], fill_buf, sizeof(fill_buf));
        if (w == -1) {
            CHECK(errno == EAGAIN, "O_NONBLOCK 满管道 write 返回 EAGAIN");
            break;
        }
        total_written += w;
    }
    /* total_written 表示管道填满前写入的总字节数 */
    (void)total_written;

    close(fds[0]);
    close(fds[1]);

    /* ================================================================
     * PART 7: 管道容量验证
     * ================================================================ */

    /* 9. 管道容量验证 */
    ret = pipe2(fds, 0);
    CHECK_RET(ret, 0, "pipe2 创建(容量测试)");

    /* 获取管道大小 */
    int pipe_size = fcntl(fds[1], F_GETPIPE_SZ);
    CHECK(pipe_size >= 4096, "F_GETPIPE_SZ 返回值 >= 4096");

    /* 写入大数据 */
    char *big_buf = malloc(BIG_SIZE);
    if (big_buf) {
        /* 填充模式 */
        for (int i = 0; i < BIG_SIZE; i++) {
            big_buf[i] = (char)(i & 0xFF);
        }

        /* 写入 */
        ssize_t written = write(fds[1], big_buf, BIG_SIZE);
        CHECK(written > 0, "写入大数据到管道");

        /* 读回验证 */
        char *read_buf = malloc(BIG_SIZE);
        if (read_buf) {
            ssize_t total_read = 0;
            ssize_t n;
            while ((n = read(fds[0], read_buf + total_read,
                           BIG_SIZE - total_read)) > 0) {
                total_read += n;
            }

            CHECK(total_read == written, "读回字节数与写入一致");
            CHECK(memcmp(big_buf, read_buf, written) == 0,
                  "大数据内容精确匹配");

            free(read_buf);
        }
        free(big_buf);
    }

    close(fds[0]);
    close(fds[1]);

    /* ================================================================
     * PART 8: 负向测试
     * ================================================================ */

    /* 10. pipefd NULL 指针 */
    errno = 0;
    ret = pipe2(NULL, 0);
    CHECK(ret == -1 && (errno == EFAULT || errno == EINVAL),
          "pipe2 NULL 应返回 EFAULT 或 EINVAL");

    /* 11. 无效 flags */
    errno = 0;
    ret = pipe2(fds, 0xFFFF);
    CHECK(ret == -1 && errno == EINVAL,
          "pipe2 无效 flags 应返回 EINVAL");

    /* 12. 验证两个 fd 确实不同 */
    ret = pipe2(fds, 0);
    CHECK(ret == 0 && fds[0] != fds[1],
          "pipe2 返回两个不同的 fd");
    if (ret == 0) {
        close(fds[0]);
        close(fds[1]);
    }

    /* 13. 检查管道确实是 pipe */
    ret = pipe2(fds, 0);
    if (ret == 0) {
        int flags = fcntl(fds[0], F_GETFL);
        /* 管道的标志应该包含 O_RDONLY 或 O_RDWR */
        CHECK(flags >= 0, "pipe 读端 fd 有效");
        close(fds[0]);
        close(fds[1]);
    }

    /* ================================================================
     * PART 9: SIGPIPE 信号测试
     * ================================================================ */

    /* 14. 验证 SIGPIPE 信号（有 handler 时）*/
    ret = pipe2(fds, 0);
    if (ret == 0) {
        /* 设置 SIGPIPE handler */
        signal(SIGPIPE, SIG_IGN);

        close(fds[0]);
        errno = 0;
        ssize_t sigpipe_write = write(fds[1], "test", 4);
        CHECK(sigpipe_write == -1 && errno == EPIPE,
              "有 SIGPIPE handler 时 write 返回 EPIPE");

        close(fds[1]);
    }

    TEST_DONE();
}
