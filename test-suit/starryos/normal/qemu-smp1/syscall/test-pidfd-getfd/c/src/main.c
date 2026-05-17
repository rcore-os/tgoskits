#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef __NR_pidfd_open
#error "__NR_pidfd_open required from <sys/syscall.h>"
#endif
#ifndef __NR_pidfd_getfd
#error "__NR_pidfd_getfd required from <sys/syscall.h>"
#endif

static int x_pidfd_open(pid_t pid, unsigned int flags)
{
    return (int)syscall(__NR_pidfd_open, pid, flags);
}

static int x_pidfd_getfd(int pidfd, int target_fd, unsigned int flags)
{
    return (int)syscall(__NR_pidfd_getfd, pidfd, target_fd, flags);
}

static int get_cloexec(int fd)
{
    int flags = fcntl(fd, F_GETFD);
    if (flags == -1) {
        return -1;
    }
    return !!(flags & FD_CLOEXEC);
}

static void test_pidfd_getfd_self_basic(void)
{
    printf("--- pidfd_getfd 正常路径 ---\n");

    int pfd = x_pidfd_open(getpid(), 0);
    CHECK(pfd >= 0, "pidfd_open(getpid(), 0) 成功");
    if (pfd < 0) {
        return;
    }

    int pipe_fds[2];
    CHECK_RET(pipe(pipe_fds), 0, "pipe 创建成功");

    const char msg[] = "pidfd-getfd";
    CHECK_RET(write(pipe_fds[1], msg, sizeof(msg) - 1), (ssize_t)(sizeof(msg) - 1),
              "pipe write");

    int dup_fd = x_pidfd_getfd(pfd, pipe_fds[0], 0);
    CHECK(dup_fd >= 0, "pidfd_getfd 返回新 fd");
    if (dup_fd >= 0) {
        char buf[32] = {0};
        ssize_t n = read(dup_fd, buf, sizeof(buf) - 1);
        CHECK(n == (ssize_t)(sizeof(msg) - 1), "dup fd read 长度正确");
        CHECK(strcmp(buf, msg) == 0, "dup fd read 内容正确");
        close(dup_fd);
    }

    close(pipe_fds[0]);
    close(pipe_fds[1]);
    close(pfd);
}

static void test_pidfd_getfd_cloexec(void)
{
    printf("--- pidfd_getfd O_CLOEXEC ---\n");

    int pfd = x_pidfd_open(getpid(), 0);
    int pipe_fds[2];
    if (pfd < 0 || pipe(pipe_fds) != 0) {
        return;
    }

    int dup_fd = x_pidfd_getfd(pfd, pipe_fds[0], 0);
    if (dup_fd >= 0) {
        CHECK(get_cloexec(dup_fd) == 1, "pidfd_getfd 返回 FD_CLOEXEC");
        close(dup_fd);
    }

    close(pipe_fds[0]);
    close(pipe_fds[1]);
    close(pfd);
}

static void test_pidfd_getfd_bad_target(void)
{
    printf("--- pidfd_getfd 无效 target_fd ---\n");

    int pfd = x_pidfd_open(getpid(), 0);
    CHECK(pfd >= 0, "pidfd_open 成功");
    if (pfd >= 0) {
        CHECK_ERR(x_pidfd_getfd(pfd, 111, 0), EBADF, "不存在 target_fd -> EBADF");
        close(pfd);
    }
}

static void test_pidfd_getfd_bad_pidfd(void)
{
    printf("--- pidfd_getfd 非 pidfd ---\n");

    int pipe_fds[2];
    CHECK_RET(pipe(pipe_fds), 0, "pipe 创建成功");
    CHECK_ERR(x_pidfd_getfd(pipe_fds[0], 0, 0), EINVAL, "普通 fd 作 pidfd -> EINVAL");
    close(pipe_fds[0]);
    close(pipe_fds[1]);
}

static void test_pidfd_getfd_nonzero_flags(void)
{
    printf("--- pidfd_getfd 非法 flags ---\n");

    int pfd = x_pidfd_open(getpid(), 0);
    CHECK(pfd >= 0, "pidfd_open 成功");
    if (pfd >= 0) {
        CHECK_ERR(x_pidfd_getfd(pfd, 0, 1), EINVAL, "flags=1 -> EINVAL");
        close(pfd);
    }
}

int main(void)
{
    TEST_START("pidfd_getfd");

    signal(SIGPIPE, SIG_IGN);

    test_pidfd_getfd_self_basic();
    test_pidfd_getfd_cloexec();
    test_pidfd_getfd_bad_target();
    test_pidfd_getfd_bad_pidfd();
    test_pidfd_getfd_nonzero_flags();

    TEST_DONE();
}
