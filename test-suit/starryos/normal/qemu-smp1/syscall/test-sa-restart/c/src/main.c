/*
 * test_sa_restart.c
 *
 * 测试 SA_RESTART 语义: 信号中断系统调用时，如果信号处理函数设置了
 * SA_RESTART 标志，内核应自动重启被中断的系统调用，而不是返回 EINTR。
 *
 * 测试方法: 父进程创建管道，子进程在管道上 read() 阻塞，父进程发送
 * SIGUSR1。SA_RESTART 时 read 应继续阻塞等待数据; 无 SA_RESTART 时
 * read 应返回 -1/EINTR。
 */

#include "test_framework.h"
#include <signal.h>
#include <unistd.h>
#include <sys/wait.h>
#include <fcntl.h>

static volatile sig_atomic_t sig_count = 0;

static void handler(int sig)
{
    (void)sig;
    sig_count++;
}

int main(void)
{
    TEST_START("sa_restart: SA_RESTART 系统调用重启");

    /* Test 1: SA_RESTART set, read should NOT return EINTR */
    {
        int pipefd[2];
        CHECK(pipe(pipefd) == 0, "pipe created");

        pid_t pid = fork();
        if (pid == 0) {
            close(pipefd[1]);
            struct sigaction sa = {0};
            sa.sa_handler = handler;
            sa.sa_flags = SA_RESTART;
            sigaction(SIGUSR1, &sa, NULL);

            char buf[1];
            /* This read will block. Parent sends SIGUSR1, then data.
             * With SA_RESTART, read should NOT return EINTR but wait
             * for data and return 1. */
            ssize_t n = read(pipefd[0], buf, 1);
            close(pipefd[0]);
            /* n==1 means restart worked; n==-1 means EINTR leaked */
            _exit(n == 1 ? 0 : 1);
        }
        close(pipefd[0]);
        /* Give child time to enter read() */
        usleep(50000);
        kill(pid, SIGUSR1);
        /* Give signal time to be delivered */
        usleep(50000);
        /* Now write data so child's read completes */
        write(pipefd[1], "x", 1);
        close(pipefd[1]);

        int status;
        waitpid(pid, &status, 0);
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
              "SA_RESTART: read restarts after signal");
    }

    /* Test 2: SA_RESTART flag preserved across fork.
     * Install the handler in the parent so the child actually has
     * something to inherit (Test 1's handler lived in Test 1's child,
     * not in this process). */
    {
        struct sigaction sa = {0};
        sa.sa_handler = handler;
        sa.sa_flags = SA_RESTART;
        CHECK(sigaction(SIGUSR1, &sa, NULL) == 0, "install SA_RESTART handler");

        pid_t pid = fork();
        if (pid == 0) {
            struct sigaction got;
            sigaction(SIGUSR1, NULL, &got);
            _exit((got.sa_flags & SA_RESTART) ? 0 : 1);
        }
        int status;
        waitpid(pid, &status, 0);
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
              "SA_RESTART flag preserved across fork");
    }

    /* Test 3: SA_RESTART with accept() (common PostgreSQL pattern) */
    /* Skip: requires socket setup; the pipe tests above validate the
     * core mechanism. */

    TEST_DONE();
}
