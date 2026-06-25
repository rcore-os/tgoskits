#define _GNU_SOURCE

/*
 * test-signal-interrupt-eintr
 *
 * 测试目的：
 * 1) 固定 StarryOS 信号打断阻塞 syscall 的 ABI 语义：
 *    线程/进程阻塞在 interruptible 路径（本例用 poll）时，
 *    收到可投递且未屏蔽信号后必须返回 -1，errno == EINTR。
 * 2) 避免仅依赖 nginx 多 worker 集成场景；
 *    一旦 task.interrupt() 语义回退，本用例应直接失败。
 */

#include <errno.h>
#include <poll.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

#define TEST_TIMEOUT_MS 5000
#define WAIT_POLL_INTERVAL_US 10000

static volatile sig_atomic_t got_usr1 = 0;

static void on_usr1(int signo)
{
    if (signo == SIGUSR1) {
        got_usr1 = 1;
    }
}

static int child_run(int notify_fd, int block_fd)
{
    /*
     * 子进程安装可投递信号处理器：
     * - 不设置 SA_RESTART，确保阻塞 syscall 被信号打断后返回 EINTR；
     * - 该语义用于固定 task.interrupt() 的回归行为。
     */
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = on_usr1;
    sigemptyset(&sa.sa_mask);
    if (sigaction(SIGUSR1, &sa, NULL) != 0) {
        perror("child: sigaction(SIGUSR1)");
        return 1;
    }

    sigset_t empty;
    sigemptyset(&empty);
    if (sigprocmask(SIG_SETMASK, &empty, NULL) != 0) {
        perror("child: sigprocmask(SIG_SETMASK)");
        return 1;
    }

    char ready = 'R';
    /* 告知父进程：子进程已完成初始化，可开始触发信号。 */
    if (write(notify_fd, &ready, 1) != 1) {
        perror("child: notify parent");
        return 1;
    }

    struct pollfd pfd = {
        .fd = block_fd,
        .events = POLLIN,
    };

    errno = 0;
    /* 关键断言：阻塞 poll 在 SIGUSR1 到达后必须返回 -1/EINTR。 */
    int r = poll(&pfd, 1, -1);
    int e = errno;
    if (r == -1 && e == EINTR && got_usr1) {
        printf("PASS: poll interrupted by SIGUSR1 with EINTR\n");
        return 0;
    }

    fprintf(stderr,
            "FAIL: poll result mismatch: ret=%d errno=%d (%s) got_usr1=%d\n",
            r, e, strerror(e), got_usr1);
    return 1;
}

int main(void)
{
    int block_pipe[2] = {-1, -1};
    int sync_pipe[2] = {-1, -1};
    if (pipe(block_pipe) != 0 || pipe(sync_pipe) != 0) {
        perror("pipe");
        return 1;
    }

    pid_t child = fork();
    if (child < 0) {
        perror("fork");
        return 1;
    }

    if (child == 0) {
        close(sync_pipe[0]);
        close(block_pipe[1]);
        int rc = child_run(sync_pipe[1], block_pipe[0]);
        close(sync_pipe[1]);
        close(block_pipe[0]);
        _exit(rc);
    }

    close(sync_pipe[1]);
    close(block_pipe[0]);

    /*
     * 父子握手同步，避免用 sleep 猜时序：
     * 只有收到子进程 ready 后，父进程才发送 SIGUSR1。
     */
    char ready = 0;
    if (read(sync_pipe[0], &ready, 1) != 1 || ready != 'R') {
        fprintf(stderr, "FAIL: parent failed to receive child ready signal\n");
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
        return 1;
    }

    if (kill(child, SIGUSR1) != 0) {
        perror("parent: kill(SIGUSR1)");
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
        return 1;
    }

    int status = 0;
    int waited_ms = 0;
    pid_t waited = 0;
    while (waited_ms < TEST_TIMEOUT_MS) {
        waited = waitpid(child, &status, WNOHANG);
        if (waited == child) {
            break;
        }
        if (waited < 0) {
            if (errno == EINTR) {
                continue;
            }
            perror("parent: waitpid");
            return 1;
        }
        usleep(WAIT_POLL_INTERVAL_US);
        waited_ms += WAIT_POLL_INTERVAL_US / 1000;
        if (kill(child, SIGUSR1) != 0) {
            if (errno == ESRCH) {
                continue;
            }
            perror("parent: retry kill(SIGUSR1)");
            kill(child, SIGKILL);
            waitpid(child, NULL, 0);
            return 1;
        }
    }

    if (waited != child) {
        fprintf(stderr, "FAIL: child did not exit after SIGUSR1 within %d ms\n",
                TEST_TIMEOUT_MS);
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
        return 1;
    }

    close(sync_pipe[0]);
    close(block_pipe[1]);

    if (WIFEXITED(status) && WEXITSTATUS(status) == 0) {
        printf("ALL TESTS PASSED\n");
        return 0;
    }

    if (WIFSIGNALED(status)) {
        fprintf(stderr, "FAIL: child killed by signal %d\n", WTERMSIG(status));
    } else if (WIFEXITED(status)) {
        fprintf(stderr, "FAIL: child exited with code %d\n", WEXITSTATUS(status));
    } else {
        fprintf(stderr, "FAIL: unexpected child wait status=0x%x\n", status);
    }
    return 1;
}
