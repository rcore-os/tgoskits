/*
 * bug-tty-sigint: 验证 kill(-pgid, SIGINT/SIGTSTP) 正确传递给进程组。
 *
 * 背景：TTY 驱动检测到 Ctrl+C (0x03) / Ctrl+Z (0x1A) 后，调用
 * send_signal_to_process_group(pgid, SIGINT/SIGTSTP)。本测试直接
 * 验证该内核路径，作为 TTY 信号传递修复的复现用例。
 *
 * 测试一：kill(-pgid, SIGINT) 终止前台阻塞进程
 *   子进程创建独立进程组，阻塞于 pause()；
 *   父进程向该进程组发 SIGINT；
 *   waitpid 后确认子进程因 SIGINT 终止。
 *
 * 测试二：kill(-pgid, SIGTSTP) 传递到进程组（自定义处理函数）
 *   子进程创建独立进程组，安装 SIGTSTP 处理函数，阻塞于 pause()；
 *   父进程向该进程组发 SIGTSTP；
 *   子进程处理函数被调用后退出 0，否则退出 1。
 */
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static volatile sig_atomic_t got_sigtstp = 0;
static void sigtstp_handler(int sig) { (void)sig; got_sigtstp = 1; }

static void sleep_ms(long ms)
{
    struct timespec ts = { ms / 1000, (ms % 1000) * 1000000L };
    nanosleep(&ts, NULL);
}

/* ---- 测试一：SIGINT 终止进程组内阻塞进程 ---- */
static int test_sigint_terminates_pgroup(void)
{
    printf("[test1] kill(-pgid, SIGINT) 应终止阻塞在 pause() 的子进程\n");

    int sync[2];
    if (pipe(sync) != 0) {
        printf("FAIL: pipe: %s\n", strerror(errno));
        return 1;
    }

    pid_t child = fork();
    if (child < 0) {
        printf("FAIL: fork: %s\n", strerror(errno));
        return 1;
    }

    if (child == 0) {
        /* 子进程：建立独立进程组 */
        close(sync[0]);
        if (setpgid(0, 0) != 0) {
            write(sync[1], "E", 1);
            _exit(99);
        }
        write(sync[1], "R", 1);
        close(sync[1]);
        pause(); /* 阻塞，等待信号 */
        _exit(0); /* 不应到达 */
    }

    /* 父进程 */
    close(sync[1]);
    char buf;
    if (read(sync[0], &buf, 1) != 1 || buf != 'R') {
        printf("FAIL: 子进程未就绪\n");
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
        return 1;
    }
    close(sync[0]);

    sleep_ms(100); /* 确保子进程进入 pause() */

    /* 向子进程的进程组发 SIGINT（与 TTY Ctrl+C 路径相同） */
    if (kill(-child, SIGINT) != 0) {
        printf("FAIL: kill(-pgid, SIGINT): %s\n", strerror(errno));
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
        return 1;
    }

    int status = 0;
    if (waitpid(child, &status, 0) != child) {
        printf("FAIL: waitpid: %s\n", strerror(errno));
        return 1;
    }

    if (WIFSIGNALED(status) && WTERMSIG(status) == SIGINT) {
        printf("PASS: 子进程因 SIGINT 终止\n");
        return 0;
    }
    if (WIFEXITED(status)) {
        printf("FAIL: 子进程正常退出 (code=%d)，应被 SIGINT 终止\n",
               WEXITSTATUS(status));
    } else {
        printf("FAIL: 子进程被信号 %d 终止，期望 SIGINT(%d)\n",
               WTERMSIG(status), SIGINT);
    }
    return 1;
}

/* ---- 测试二：SIGTSTP 传递到进程组（自定义处理函数） ---- */
static int test_sigtstp_delivered_to_pgroup(void)
{
    printf("[test2] kill(-pgid, SIGTSTP) 应传递到进程组内阻塞进程\n");

    int sync[2];
    if (pipe(sync) != 0) {
        printf("FAIL: pipe: %s\n", strerror(errno));
        return 1;
    }

    pid_t child = fork();
    if (child < 0) {
        printf("FAIL: fork: %s\n", strerror(errno));
        return 1;
    }

    if (child == 0) {
        close(sync[0]);
        if (setpgid(0, 0) != 0) {
            write(sync[1], "E", 1);
            _exit(99);
        }

        /* 安装 SIGTSTP 处理函数（不使用默认的 Stop 动作） */
        struct sigaction sa = {0};
        sa.sa_handler = sigtstp_handler;
        sigemptyset(&sa.sa_mask);
        sigaction(SIGTSTP, &sa, NULL);

        write(sync[1], "R", 1);
        close(sync[1]);

        pause(); /* 等待 SIGTSTP */

        /* 处理函数被调用后 pause() 返回 -1/EINTR */
        _exit(got_sigtstp ? 0 : 1);
    }

    close(sync[1]);
    char buf;
    if (read(sync[0], &buf, 1) != 1 || buf != 'R') {
        printf("FAIL: 子进程未就绪\n");
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
        return 1;
    }
    close(sync[0]);

    sleep_ms(100);

    if (kill(-child, SIGTSTP) != 0) {
        printf("FAIL: kill(-pgid, SIGTSTP): %s\n", strerror(errno));
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
        return 1;
    }

    int status = 0;
    if (waitpid(child, &status, 0) != child) {
        printf("FAIL: waitpid: %s\n", strerror(errno));
        return 1;
    }

    if (WIFEXITED(status) && WEXITSTATUS(status) == 0) {
        printf("PASS: 子进程收到 SIGTSTP 并正常退出\n");
        return 0;
    }
    if (WIFEXITED(status)) {
        printf("FAIL: 子进程退出 code=%d（未收到 SIGTSTP？）\n",
               WEXITSTATUS(status));
    } else {
        printf("FAIL: 子进程被信号 %d 终止\n", WTERMSIG(status));
    }
    return 1;
}

int main(void)
{
    printf("=== bug-tty-sigint ===\n");
    printf("验证 kill(-pgid) 信号传递路径（TTY Ctrl+C/Ctrl+Z 修复复现）\n\n");

    int r1 = test_sigint_terminates_pgroup();
    int r2 = test_sigtstp_delivered_to_pgroup();

    printf("\n");
    if (r1 == 0 && r2 == 0) {
        printf("TEST PASSED\n");
        return 0;
    }
    printf("TEST FAILED\n");
    return 1;
}
