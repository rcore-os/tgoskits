/*
 * !test-splice-carpet — splice(2) 零拷贝穷尽测试(浏览器/nginx 大文件/流转发命脉)
 *
 * ground truth: man 2 splice + Linux v7.2 fs/splice.c。至少一端必须是 pipe。
 * 覆盖: pipe<->file / pipe<->pipe 数据搬运 + off_in/off_out + errno
 * (EBADF/EINVAL 两端非pipe或未知flag/ESPIPE pipe带offset/EAGAIN 非阻塞空pipe)。
 *
 * =====================================================================
 * 语义 (man 2 splice)
 * =====================================================================
 *   ssize_t splice(fd_in, off_in, fd_out, off_out, len, flags);
 *   在两个 fd 间移动数据, 至少一端为 pipe; 返回搬运字节数。
 *   pipe 端的 offset 必须为 NULL(否则 ESPIPE); 非 pipe 端可给 offset。
 *   两端都非 pipe -> EINVAL; 未知 flag -> EINVAL; SPLICE_F_NONBLOCK 空 pipe -> EAGAIN。
 *
 * =====================================================================
 * Linux/StarryOS 对齐 (fs/splice.c / syscall/fs/io.rs sys_splice)
 * =====================================================================
 *   io.rs:864 sys_splice: flag 校验/EBADF/EINVAL neither-pipe/ESPIPE/O_APPEND 拒/SendFile 传输。
 * =====================================================================
 */

#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
#include "test_framework.h"

#include <stddef.h>
#include <stdint.h>
#include <fcntl.h>
#include <signal.h>
#include <unistd.h>
#include <string.h>

#ifndef SPLICE_F_NONBLOCK
#define SPLICE_F_MOVE 1
#define SPLICE_F_NONBLOCK 2
#define SPLICE_F_MORE 4
#endif

static void alarm_handler(int s)
{
    (void)s;
    const char *m = "\n  FAIL | TIMEOUT | 测试挂死\n==== test-splice-carpet 汇总: FAIL ====\n";
    ssize_t r = write(2, m, strlen(m));
    (void)r;
    _exit(1);
}

static const char *TMPF = "/tmp/splice_carpet.dat";

/* ===== A. pipe -> file: 写 pipe, splice 到文件, 校验内容 ===== */
static int test_pipe_to_file(void)
{
    TEST_START("A. splice pipe -> file");
    int pfd[2];
    if (pipe(pfd) != 0) { CHECK(0, "pipe"); TEST_DONE(); }
    int fd = open(TMPF, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) { CHECK(0, "open tmp"); close(pfd[0]); close(pfd[1]); TEST_DONE(); }

    const char *data = "splice-zero-copy-payload";
    size_t dl = strlen(data);
    CHECK(write(pfd[1], data, dl) == (ssize_t)dl, "写 pipe");

    ssize_t n = splice(pfd[0], NULL, fd, NULL, dl, 0);
    CHECK(n == (ssize_t)dl, "splice pipe->file 搬运全部字节");
    /* 读回文件校验 */
    lseek(fd, 0, SEEK_SET);
    char buf[64] = {0};
    ssize_t rn = read(fd, buf, sizeof(buf));
    CHECK(rn == (ssize_t)dl && memcmp(buf, data, dl) == 0, "文件内容 == pipe 数据");

    close(pfd[0]); close(pfd[1]); close(fd);
    TEST_DONE();
}

/* ===== B. file -> pipe: splice 文件到 pipe, 读回校验(带 off_in) ===== */
static int test_file_to_pipe(void)
{
    TEST_START("B. splice file -> pipe(带 off_in)");
    int fd = open(TMPF, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) { CHECK(0, "open"); TEST_DONE(); }
    const char *data = "ABCDEFGHIJ0123456789";
    write(fd, data, strlen(data));

    int pfd[2];
    if (pipe(pfd) != 0) { CHECK(0, "pipe"); close(fd); TEST_DONE(); }

    /* off_in=5: 从文件偏移 5 开始搬 5 字节 -> "FGHIJ" */
    off_t off = 5;
    ssize_t n = splice(fd, &off, pfd[1], NULL, 5, 0);
    CHECK(n == 5, "splice file->pipe 搬 5 字节");
    CHECK(off == 10, "off_in 前进到 10(消费 5 字节)");
    char buf[16] = {0};
    ssize_t rn = read(pfd[0], buf, sizeof(buf));
    CHECK(rn == 5 && memcmp(buf, "FGHIJ", 5) == 0, "pipe 收到文件偏移 5 的数据");

    close(pfd[0]); close(pfd[1]); close(fd);
    TEST_DONE();
}

/* ===== C. pipe -> pipe ===== */
static int test_pipe_to_pipe(void)
{
    TEST_START("C. splice pipe -> pipe");
    int a[2], b[2];
    if (pipe(a) != 0 || pipe(b) != 0) { CHECK(0, "pipe"); TEST_DONE(); }
    const char *data = "pipe2pipe";
    size_t dl = strlen(data);
    write(a[1], data, dl);
    ssize_t n = splice(a[0], NULL, b[1], NULL, dl, 0);
    CHECK(n == (ssize_t)dl, "splice pipe->pipe 搬运");
    char buf[16] = {0};
    CHECK(read(b[0], buf, sizeof(buf)) == (ssize_t)dl && memcmp(buf, data, dl) == 0,
          "第二个 pipe 收到数据");
    close(a[0]); close(a[1]); close(b[0]); close(b[1]);
    TEST_DONE();
}

/* ===== D. errno ===== */
static int test_errno(void)
{
    TEST_START("D. splice errno(EINVAL/ESPIPE/EBADF/EAGAIN)");
    int pfd[2];
    if (pipe(pfd) != 0) { CHECK(0, "pipe"); TEST_DONE(); }
    int fd = open(TMPF, O_RDWR | O_CREAT | O_TRUNC, 0644);
    write(fd, "xxxxx", 5);
    lseek(fd, 0, SEEK_SET);

    /* 两端都非 pipe -> EINVAL。fd_out 必须可写(否则 EBADF 优先), 故用 O_RDWR 打开。 */
    int fd2 = open(TMPF, O_RDWR);
    errno = 0;
    CHECK(splice(fd, NULL, fd2, NULL, 4, 0) == -1 && errno == EINVAL, "两端非 pipe -> EINVAL");
    close(fd2);

    /* 未知 flag -> EINVAL */
    errno = 0;
    CHECK(splice(pfd[0], NULL, fd, NULL, 4, 0x80000000) == -1 && errno == EINVAL,
          "未知 flag -> EINVAL");

    /* pipe 端带 offset -> ESPIPE */
    off_t off = 0;
    errno = 0;
    CHECK(splice(pfd[0], &off, fd, NULL, 4, 0) == -1 && errno == ESPIPE,
          "pipe 端带 offset -> ESPIPE");

    /* bad fd -> EBADF */
    errno = 0;
    CHECK(splice(999, NULL, pfd[1], NULL, 4, 0) == -1 && errno == EBADF, "bad fd -> EBADF");

    /* SPLICE_F_NONBLOCK 空 pipe -> EAGAIN */
    int e[2];
    if (pipe(e) == 0) {
        errno = 0;
        ssize_t r = splice(e[0], NULL, fd, NULL, 4, SPLICE_F_NONBLOCK);
        CHECK(r == -1 && (errno == EAGAIN || errno == EWOULDBLOCK),
              "SPLICE_F_NONBLOCK 空 pipe -> EAGAIN");
        close(e[0]); close(e[1]);
    }

    close(pfd[0]); close(pfd[1]); close(fd);
    TEST_DONE();
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    signal(SIGPIPE, SIG_IGN);
    signal(SIGALRM, alarm_handler);
    alarm(60);
    int fail = 0;
    fail |= test_pipe_to_file();
    fail |= test_file_to_pipe();
    fail |= test_pipe_to_pipe();
    fail |= test_errno();
    unlink(TMPF);
    printf("\n==== test-splice-carpet 汇总: %s ====\n", fail ? "FAIL" : "PASS");
    return fail;
}
