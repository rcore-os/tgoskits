#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <signal.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef __NR_pidfd_open
#error "__NR_pidfd_open required"
#endif
#ifndef __NR_pidfd_getfd
#error "__NR_pidfd_getfd required"
#endif

static int x_pidfd_open(pid_t pid, unsigned int flags)
{
    return (int)syscall(__NR_pidfd_open, pid, flags);
}

static int x_pidfd_getfd(int pidfd, int targetfd, unsigned int flags)
{
    return (int)syscall(__NR_pidfd_getfd, pidfd, targetfd, flags);
}

static void test_pidfd_getfd_flags(void)
{
    printf("--- pidfd_getfd 非法 flags ---\n");

    errno = 0;
    int pfd = x_pidfd_open(getpid(), 0);
    CHECK(pfd >= 0, "pidfd_open(self) 成功");
    if (pfd < 0) {
        return;
    }

    CHECK_ERR(x_pidfd_getfd(pfd, 0, 1u), EINVAL, "flags 非零 -> EINVAL");

    CHECK_RET(close(pfd), 0, "close pidfd");
}

static void test_pidfd_getfd_cross_process(void)
{
    printf("--- pidfd_getfd 跨进程 pipe ---\n");

    int c2p[2];
    int p2c[2];
    CHECK_RET(pipe(c2p), 0, "pipe c2p");
    CHECK_RET(pipe(p2c), 0, "pipe p2c");

    pid_t cpid = fork();
    CHECK(cpid >= 0, "fork 成功");

    if (cpid == 0) {
        close(c2p[0]);
        close(p2c[1]);

        int data[2];
        if (pipe(data) != 0) {
            _exit(21);
        }

        int rd = data[0];
        int wr = data[1];
        if (write(c2p[1], &wr, sizeof(wr)) != (ssize_t)sizeof(wr)) {
            _exit(22);
        }

        char ack;
        if (read(p2c[0], &ack, 1) != 1) {
            _exit(23);
        }

        char buf[8] = {0};
        ssize_t n = read(rd, buf, sizeof(buf) - 1);
        close(rd);
        close(wr);
        close(c2p[1]);
        close(p2c[0]);
        if (n != 2 || buf[0] != 'H' || buf[1] != 'I') {
            _exit(24);
        }
        _exit(0);
    }

    close(c2p[1]);
    close(p2c[0]);

    int child_wr = -1;
    CHECK((ssize_t)read(c2p[0], &child_wr, sizeof(child_wr)) == (ssize_t)sizeof(child_wr),
          "读取子进程 target fd 编号");
    close(c2p[0]);

    errno = 0;
    int pidfd = x_pidfd_open(cpid, 0);
    CHECK(pidfd >= 0, "pidfd_open(child) 成功");
    if (pidfd < 0) {
        char z = 0;
        write(p2c[1], &z, 1);
        waitpid(cpid, NULL, 0);
        close(p2c[1]);
        return;
    }

    errno = 0;
    int dupfd = x_pidfd_getfd(pidfd, child_wr, 0);
    CHECK(dupfd >= 0, "pidfd_getfd 成功");
    if (dupfd < 0) {
        char z = 0;
        write(p2c[1], &z, 1);
        waitpid(cpid, NULL, 0);
        close(pidfd);
        close(p2c[1]);
        return;
    }

    const char *out = "HI";
    CHECK((ssize_t)write(dupfd, out, 2) == 2, "向 dup 的 pipe 写端写入");

    char go = 1;
    CHECK_RET(write(p2c[1], &go, 1), 1, "通知子进程开始读");

    close(dupfd);
    close(pidfd);
    close(p2c[1]);

    int st = 0;
    CHECK_RET(waitpid(cpid, &st, 0), cpid, "waitpid 子进程");
    CHECK(WIFEXITED(st) && WEXITSTATUS(st) == 0, "子进程校验读到的数据");
}

int main(void)
{
    TEST_START("pidfd_getfd");

    signal(SIGPIPE, SIG_IGN);

    test_pidfd_getfd_flags();
    test_pidfd_getfd_cross_process();

    TEST_DONE();
}
