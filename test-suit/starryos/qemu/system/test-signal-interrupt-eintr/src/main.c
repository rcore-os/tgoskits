#define _GNU_SOURCE

/*
 * test-signal-interrupt-eintr
 *
 * 测试目的：
 * 1) 固定 StarryOS 信号打断阻塞 syscall 的 ABI 语义：
 *    线程/进程阻塞在 interruptible 路径（本例用 ppoll）时，
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

static int read_ready_byte(int fd, char *ready)
{
    for (;;) {
        ssize_t n = read(fd, ready, 1);
        if (n == 1) {
            return 0;
        }
        if (n == 0) {
            fprintf(stderr, "FAIL: parent read child ready pipe: EOF\n");
            return -1;
        }
        if (errno == EINTR) {
            continue;
        }
        fprintf(stderr, "FAIL: parent read child ready pipe: errno=%d (%s)\n",
                errno, strerror(errno));
        return -1;
    }
}

static int child_run(int notify_fd, int release_fd, int block_fd)
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

    sigset_t blocked;
    sigemptyset(&blocked);
    sigaddset(&blocked, SIGUSR1);
    if (sigprocmask(SIG_BLOCK, &blocked, NULL) != 0) {
        perror("child: sigprocmask(SIG_BLOCK)");
        return 1;
    }

    char ready = 'R';
    /* 告知父进程：子进程已完成初始化，可开始触发信号。 */
    if (write(notify_fd, &ready, 1) != 1) {
        perror("child: notify parent");
        return 1;
    }

    /*
     * Keep SIGUSR1 blocked until the parent confirms that kill() completed.
     * The signal is therefore pending before ppoll atomically installs its
     * empty mask, eliminating the ready-to-wait userspace race.
     */
    char release = 0;
    if (read(release_fd, &release, 1) != 1 || release != 'G') {
        perror("child: wait for parent release");
        return 1;
    }

    struct pollfd pfd = {
        .fd = block_fd,
        .events = POLLIN,
    };

    errno = 0;
    sigset_t wait_mask;
    sigemptyset(&wait_mask);
    /*
     * The temporary empty mask makes the already-pending SIGUSR1 deliverable
     * atomically with entering the wait. ppoll must return -1/EINTR.
     */
    int r = ppoll(&pfd, 1, NULL, &wait_mask);
    int e = errno;
    if (r == -1 && e == EINTR && got_usr1) {
        sigset_t restored;
        if (sigprocmask(SIG_SETMASK, NULL, &restored) != 0) {
            perror("child: read restored signal mask");
            return 1;
        }
        if (sigismember(&restored, SIGUSR1) != 1) {
            fprintf(stderr,
                    "FAIL: ppoll did not restore the caller's SIGUSR1 mask\n");
            return 1;
        }
        printf("PASS: ppoll interrupted by pending SIGUSR1 with EINTR\n");
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
    int release_pipe[2] = {-1, -1};
    if (pipe(block_pipe) != 0 || pipe(sync_pipe) != 0 || pipe(release_pipe) != 0) {
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
        close(release_pipe[1]);
        close(block_pipe[1]);
        int rc = child_run(sync_pipe[1], release_pipe[0], block_pipe[0]);
        close(sync_pipe[1]);
        close(release_pipe[0]);
        close(block_pipe[0]);
        _exit(rc);
    }

    close(sync_pipe[1]);
    close(release_pipe[0]);
    close(block_pipe[0]);

    /*
     * 父子握手同步，避免用 sleep 猜时序：
     * 只有收到子进程 ready 后，父进程才发送 SIGUSR1。
     */
    char ready = 0;
    if (read_ready_byte(sync_pipe[0], &ready) != 0 || ready != 'R') {
        fprintf(stderr, "FAIL: parent failed to receive child ready signal\n");
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
        return 1;
    }

    /*
     * One synchronized delivery is sufficient to verify EINTR. Repeated
     * delivery races handler return and process exit, turning this into an
     * unrelated nested-signal/rt_sigreturn stress test.
     */
    if (kill(child, SIGUSR1) != 0) {
        perror("parent: kill(SIGUSR1)");
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
        return 1;
    }
    char release = 'G';
    if (write(release_pipe[1], &release, 1) != 1) {
        perror("parent: release child");
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
        return 1;
    }
    close(release_pipe[1]);

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
