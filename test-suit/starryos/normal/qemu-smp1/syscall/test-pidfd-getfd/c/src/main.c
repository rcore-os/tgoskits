#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/wait.h>
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

static int read_exact(int fd, void *buf, size_t len)
{
    unsigned char *p = buf;
    size_t left = len;

    while (left > 0) {
        ssize_t n = read(fd, p, left);
        if (n < 0) {
            return -1;
        }
        if (n == 0) {
            return -1;
        }
        p += (size_t)n;
        left -= (size_t)n;
    }
    return 0;
}

/* Child blocks on sync[0] until parent writes one byte to sync[1]. */
static int open_pidfd_before_child_exit(pid_t child, int sync[2], int *out_pfd)
{
    char ch = 0;

    *out_pfd = x_pidfd_open(child, 0);
    if (*out_pfd < 0) {
        return -1;
    }
    if (write(sync[1], &ch, 1) != 1) {
        close(*out_pfd);
        return -1;
    }
    return 0;
}

static void test_pidfd_getfd_bad_pidfd_closed(void)
{
    printf("--- pidfd_getfd 已关闭 pidfd ---\n");

    int pfd = x_pidfd_open(getpid(), 0);
    CHECK(pfd >= 0, "pidfd_open 成功");
    if (pfd < 0) {
        return;
    }
    close(pfd);
    CHECK_ERR(x_pidfd_getfd(pfd, 0, 0), EBADF, "已 close pidfd -> EBADF");
}

static void test_pidfd_getfd_bad_pidfd_minus_one(void)
{
    printf("--- pidfd_getfd pidfd=-1 ---\n");

    CHECK_ERR(x_pidfd_getfd(-1, 0, 0), EBADF, "pidfd=-1 -> EBADF");
}

static void test_pidfd_getfd_bad_pidfd(void)
{
    printf("--- pidfd_getfd 非 pidfd ---\n");

    int pipe_fds[2];
    CHECK_RET(pipe(pipe_fds), 0, "pipe 创建成功");
    errno = 0;
    if (x_pidfd_getfd(pipe_fds[0], 0, 0) == -1 && (errno == EINVAL || errno == EBADF)) {
        CHECK(1, "普通 fd 作 pidfd -> EINVAL/EBADF");
    } else {
        CHECK(0, "普通 fd 作 pidfd -> EINVAL/EBADF");
    }
    close(pipe_fds[0]);
    close(pipe_fds[1]);
}

static void test_pidfd_getfd_reaped_target(void)
{
    printf("--- pidfd_getfd reap 后目标进程 ---\n");

    int sync[2];
    if (pipe(sync) != 0) {
        return;
    }

    pid_t child = fork();
    CHECK(child >= 0, "fork 成功");
    if (child < 0) {
        close(sync[0]);
        close(sync[1]);
        return;
    }

    if (child == 0) {
        char ch;
        close(sync[1]);
        if (read(sync[0], &ch, 1) != 1) {
            _exit(1);
        }
        close(sync[0]);
        _exit(0);
    }

    close(sync[0]);
    int pfd = -1;
    CHECK(open_pidfd_before_child_exit(child, sync, &pfd) == 0, "reap 前 pidfd_open 成功");
    if (pfd < 0) {
        close(sync[1]);
        waitpid(child, NULL, 0);
        return;
    }

    int status = 0;
    CHECK_RET(waitpid(child, &status, 0), child, "waitpid reap 子进程");

    CHECK_ERR(x_pidfd_getfd(pfd, 0, 0), ESRCH, "reap 后 target_fd=0 -> ESRCH");
    CHECK_ERR(x_pidfd_getfd(pfd, -1, 0), ESRCH, "reap 后 target_fd=-1 -> ESRCH");
    close(pfd);
    close(sync[1]);
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

static void test_pidfd_getfd_negative_target_fd(void)
{
    printf("--- pidfd_getfd target_fd 为负 ---\n");

    int pfd = x_pidfd_open(getpid(), 0);
    CHECK(pfd >= 0, "pidfd_open 成功");
    if (pfd >= 0) {
        CHECK_ERR(x_pidfd_getfd(pfd, -1, 0), EBADF, "target_fd=-1 -> EBADF");
        close(pfd);
    }
}

static void test_pidfd_getfd_nonzero_flags(void)
{
    printf("--- pidfd_getfd 非法 flags ---\n");

    int pfd = x_pidfd_open(getpid(), 0);
    CHECK(pfd >= 0, "pidfd_open 成功");
    if (pfd >= 0) {
        CHECK_ERR(x_pidfd_getfd(pfd, 0, 1), EINVAL, "flags=1 -> EINVAL");
        CHECK_ERR(x_pidfd_getfd(pfd, 0, 0xffffffffu), EINVAL, "flags=0xffffffff -> EINVAL");
        close(pfd);
    }
}

static void test_pidfd_getfd_cloexec(void)
{
    printf("--- pidfd_getfd FD_CLOEXEC ---\n");

    int pfd = x_pidfd_open(getpid(), 0);
    int pipe_fds[2];
    if (pfd < 0 || pipe(pipe_fds) != 0) {
        if (pfd >= 0) {
            close(pfd);
        }
        return;
    }

    CHECK(get_cloexec(pipe_fds[0]) == 0, "原 fd 无 FD_CLOEXEC");

    int dup_fd = x_pidfd_getfd(pfd, pipe_fds[0], 0);
    if (dup_fd >= 0) {
        CHECK(get_cloexec(dup_fd) == 1, "pidfd_getfd 返回 FD_CLOEXEC");
        CHECK(get_cloexec(pipe_fds[0]) == 0, "原 fd FD_CLOEXEC 未被修改");
        close(dup_fd);
    }

    close(pipe_fds[0]);
    close(pipe_fds[1]);
    close(pfd);
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

static void test_pidfd_getfd_cross_cred_eperm(void)
{
    printf("--- pidfd_getfd 跨 cred -> EPERM ---\n");

    int notify[2];
    int release[2];
    if (pipe(notify) != 0 || pipe(release) != 0) {
        return;
    }

    pid_t child = fork();
    CHECK(child >= 0, "fork 成功");
    if (child < 0) {
        close(notify[0]);
        close(notify[1]);
        close(release[0]);
        close(release[1]);
        return;
    }

    if (child == 0) {
        int data[2];
        int target_fd;
        char ack;

        close(notify[0]);
        close(release[1]);
        if (setresuid(2000, 2000, 2000) != 0) {
            _exit(1);
        }
        if (pipe(data) != 0) {
            _exit(1);
        }
        target_fd = data[0];
        if (write(notify[1], &target_fd, sizeof(target_fd)) != (ssize_t)sizeof(target_fd)) {
            _exit(1);
        }
        close(notify[1]);
        if (read(release[0], &ack, 1) != 1) {
            _exit(1);
        }
        close(release[0]);
        close(data[0]);
        close(data[1]);
        _exit(0);
    }

    close(notify[1]);
    close(release[0]);

    if (geteuid() == 0 && setresuid(1000, 1000, 1000) != 0) {
        CHECK(0, "parent setresuid(1000) 失败");
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
        close(notify[0]);
        close(release[1]);
        return;
    }

    int child_fd = -1;
    if (read_exact(notify[0], &child_fd, sizeof(child_fd)) != 0) {
        CHECK(0, "从子进程读取 target_fd 失败");
        close(notify[0]);
        close(release[1]);
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
        return;
    }
    close(notify[0]);

    int pfd = x_pidfd_open(child, 0);
    CHECK(pfd >= 0, "pidfd_open(child) 成功");
    if (pfd < 0) {
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
        close(release[1]);
        return;
    }

    CHECK_ERR(x_pidfd_getfd(pfd, child_fd, 0), EPERM,
              "uid 1000 对 uid 2000 子进程 pidfd_getfd -> EPERM");

    char ack = 'x';
    (void)write(release[1], &ack, 1);
    close(pfd);
    close(release[1]);
    CHECK_RET(waitpid(child, NULL, 0), child, "waitpid 子进程");
}

static void test_pidfd_getfd_child_process(void)
{
    printf("--- pidfd_getfd 跨进程 dup ---\n");

    int notify[2];
    int release[2];
    if (pipe(notify) != 0 || pipe(release) != 0) {
        return;
    }

    const char msg[] = "pidfd-child";
    pid_t child = fork();
    CHECK(child >= 0, "fork 成功");
    if (child < 0) {
        close(notify[0]);
        close(notify[1]);
        close(release[0]);
        close(release[1]);
        return;
    }

    if (child == 0) {
        int data[2];
        char ack;
        int target_fd;

        close(notify[0]);
        close(release[1]);
        if (pipe(data) != 0) {
            _exit(1);
        }
        target_fd = data[0];
        if (write(notify[1], &target_fd, sizeof(target_fd)) != (ssize_t)sizeof(target_fd)) {
            _exit(1);
        }
        if (write(data[1], msg, sizeof(msg) - 1) != (ssize_t)(sizeof(msg) - 1)) {
            _exit(1);
        }
        close(notify[1]);
        if (read(release[0], &ack, 1) != 1) {
            _exit(1);
        }
        close(release[0]);
        close(data[0]);
        close(data[1]);
        _exit(0);
    }

    close(notify[1]);
    close(release[0]);

    int child_fd = -1;
    if (read_exact(notify[0], &child_fd, sizeof(child_fd)) != 0) {
        CHECK(0, "从子进程读取 target_fd 失败");
        close(notify[0]);
        close(release[1]);
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
        return;
    }
    close(notify[0]);

    int pfd = x_pidfd_open(child, 0);
    CHECK(pfd >= 0, "pidfd_open(child) 成功");
    if (pfd < 0) {
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
        close(release[1]);
        return;
    }

    int dup_fd = x_pidfd_getfd(pfd, child_fd, 0);
    CHECK(dup_fd >= 0, "pidfd_getfd(child fd) 成功");
    if (dup_fd >= 0) {
        char buf[32] = {0};
        ssize_t n = read(dup_fd, buf, sizeof(buf) - 1);
        CHECK(n == (ssize_t)(sizeof(msg) - 1), "跨进程 dup read 长度正确");
        CHECK(strcmp(buf, msg) == 0, "跨进程 dup read 内容正确");
        close(dup_fd);
    }

    char ack = 'x';
    CHECK(write(release[1], &ack, 1) == 1, "放行子进程退出");

    close(pfd);
    close(release[1]);
    CHECK_RET(waitpid(child, NULL, 0), child, "waitpid 子进程");
}

int main(void)
{
    TEST_START("pidfd_getfd");

    signal(SIGPIPE, SIG_IGN);

    test_pidfd_getfd_bad_pidfd_closed();
    test_pidfd_getfd_bad_pidfd_minus_one();
    test_pidfd_getfd_bad_pidfd();
    test_pidfd_getfd_reaped_target();
    test_pidfd_getfd_bad_target();
    test_pidfd_getfd_negative_target_fd();
    test_pidfd_getfd_nonzero_flags();
    test_pidfd_getfd_cloexec();
    test_pidfd_getfd_self_basic();
    test_pidfd_getfd_cross_cred_eperm();
    test_pidfd_getfd_child_process();

    TEST_DONE();
}
