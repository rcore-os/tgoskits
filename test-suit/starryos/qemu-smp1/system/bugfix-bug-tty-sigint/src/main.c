/*
 * bug-tty-sigint: 通过 PTY 写入 0x03/0x1a 验证 TTY 控制字符路径。
 *
 * 本测试覆盖本 PR 修复的实际路径：
 *
 *   write(master_fd, "\x03", 1)
 *     → PtyWriter 写入 master_to_slave 缓冲区，唤醒 poll_rx_slave
 *     → 从设备 InterruptDriven reader 任务唤醒
 *     → drain_source_into_line_buffer() 读取 \x03
 *     → check_send_signal() → signo_for(0x03) → SIGINT
 *     → send_signal_to_process_group(pgid, SIGINT)
 *     → 前台进程组收到 SIGINT
 *
 * 同理 \x1a → SIGTSTP（覆盖本 PR 新增的 VSUSP 映射）。
 *
 * 如果将 polling 改回 Manual 模式、或删除 signo_for 中的 VINTR/VSUSP
 * 映射，本测试将失败。
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static volatile sig_atomic_t got_sig = 0;
static void sig_handler(int s) { (void)s; got_sig = 1; }

static void sleep_ms(long ms)
{
    struct timespec ts = { ms / 1000, (ms % 1000) * 1000000L };
    nanosleep(&ts, NULL);
}

/* 打开 PTY 对，返回 master_fd；通过 slave_path 返回从设备路径 */
static int open_pty(char *slave_path, size_t len)
{
    int master = open("/dev/ptmx", O_RDWR | O_NOCTTY);
    if (master < 0) {
        printf("FAIL: open /dev/ptmx: %s\n", strerror(errno));
        return -1;
    }
    unsigned int pty_num = 0;
    if (ioctl(master, TIOCGPTN, &pty_num) != 0) {
        printf("FAIL: TIOCGPTN: %s\n", strerror(errno));
        close(master);
        return -1;
    }
    snprintf(slave_path, len, "/dev/pts/%u", pty_num);
    return master;
}

/* ---- 测试一：PTY master 写入 \x03 → 从设备前台进程收到 SIGINT ---- */
static int test_pty_ctrl_c(void)
{
    printf("[test1] PTY: write(master, \\x03) 应通过行规程发送 SIGINT\n");

    char slave_path[64];
    int master = open_pty(slave_path, sizeof(slave_path));
    if (master < 0) return 1;

    int slave = open(slave_path, O_RDWR | O_NOCTTY);
    if (slave < 0) {
        printf("FAIL: open %s: %s\n", slave_path, strerror(errno));
        close(master);
        return 1;
    }

    int sync[2];
    if (pipe(sync) != 0) { close(master); close(slave); return 1; }

    pid_t child = fork();
    if (child < 0) {
        printf("FAIL: fork: %s\n", strerror(errno));
        close(master); close(slave); close(sync[0]); close(sync[1]);
        return 1;
    }

    if (child == 0) {
        close(master);
        close(sync[0]);

        /*
         * 创建新会话并将从设备设为控制终端。
         * TIOCSCTTY → bind_to() → set_session() + set_foreground(当前进程组)
         * 此后当行规程检测到 VINTR 时，会向该前台进程组发 SIGINT。
         */
        if (setsid() < 0) { write(sync[1], "E", 1); _exit(99); }
        if (ioctl(slave, TIOCSCTTY, 0) != 0) {
            write(sync[1], "E", 1);
            _exit(99);
        }
        close(slave);

        write(sync[1], "R", 1);
        close(sync[1]);

        /* 阻塞；SIGINT 默认动作为终止进程 */
        pause();
        _exit(0); /* 不应到达 */
    }

    /* 父进程 */
    close(slave);
    close(sync[1]);
    char buf;
    if (read(sync[0], &buf, 1) != 1 || buf != 'R') {
        printf("FAIL: 子进程未就绪 (buf=%c)\n", buf);
        kill(child, SIGKILL); waitpid(child, NULL, 0);
        close(master); close(sync[0]);
        return 1;
    }
    close(sync[0]);

    sleep_ms(150); /* 确保子进程进入 pause() */

    /*
     * 写入 Ctrl+C (0x03) 到 PTY master。
     * master 的 write_at 以 is_ptm=true 直接写入 master_to_slave 缓冲区，
     * 唤醒从设备 InterruptDriven reader，触发 check_send_signal → SIGINT。
     */
    if (write(master, "\x03", 1) != 1) {
        printf("FAIL: write \\x03 to master: %s\n", strerror(errno));
        kill(child, SIGKILL); waitpid(child, NULL, 0);
        close(master);
        return 1;
    }
    close(master);

    int status = 0;
    if (waitpid(child, &status, 0) != child) {
        printf("FAIL: waitpid: %s\n", strerror(errno));
        return 1;
    }

    if (WIFSIGNALED(status) && WTERMSIG(status) == SIGINT) {
        printf("PASS: PTY 行规程正确将 \\x03 转换为 SIGINT 并终止前台进程\n");
        return 0;
    }
    if (WIFEXITED(status))
        printf("FAIL: 子进程正常退出 code=%d，期望被 SIGINT 终止\n",
               WEXITSTATUS(status));
    else
        printf("FAIL: 子进程被信号 %d 终止，期望 SIGINT(%d)\n",
               WTERMSIG(status), SIGINT);
    return 1;
}

/* ---- 测试二：PTY master 写入 \x1a → 从设备前台进程收到 SIGTSTP ---- */
static int test_pty_ctrl_z(void)
{
    printf("[test2] PTY: write(master, \\x1a) 应通过行规程发送 SIGTSTP\n");

    char slave_path[64];
    int master = open_pty(slave_path, sizeof(slave_path));
    if (master < 0) return 1;

    int slave = open(slave_path, O_RDWR | O_NOCTTY);
    if (slave < 0) {
        printf("FAIL: open %s: %s\n", slave_path, strerror(errno));
        close(master);
        return 1;
    }

    int sync[2];
    if (pipe(sync) != 0) { close(master); close(slave); return 1; }

    pid_t child = fork();
    if (child < 0) {
        printf("FAIL: fork: %s\n", strerror(errno));
        close(master); close(slave); close(sync[0]); close(sync[1]);
        return 1;
    }

    if (child == 0) {
        close(master);
        close(sync[0]);

        if (setsid() < 0) { write(sync[1], "E", 1); _exit(99); }
        if (ioctl(slave, TIOCSCTTY, 0) != 0) {
            write(sync[1], "E", 1);
            _exit(99);
        }
        close(slave);

        /*
         * 安装自定义 SIGTSTP 处理函数（覆盖默认 Stop 动作），
         * 验证信号确实经行规程投递。
         */
        struct sigaction sa = {0};
        sa.sa_handler = sig_handler;
        sigemptyset(&sa.sa_mask);
        sigaction(SIGTSTP, &sa, NULL);
        got_sig = 0;

        write(sync[1], "R", 1);
        close(sync[1]);

        pause(); /* 等待 SIGTSTP；处理函数返回后 pause() 以 EINTR 返回 */
        _exit(got_sig ? 0 : 1);
    }

    close(slave);
    close(sync[1]);
    char buf;
    if (read(sync[0], &buf, 1) != 1 || buf != 'R') {
        printf("FAIL: 子进程未就绪\n");
        kill(child, SIGKILL); waitpid(child, NULL, 0);
        close(master); close(sync[0]);
        return 1;
    }
    close(sync[0]);

    sleep_ms(150);

    /*
     * 写入 Ctrl+Z (0x1a) 到 PTY master。
     * 行规程: check_send_signal → signo_for(0x1a=VSUSP) → SIGTSTP
     * （覆盖本 PR 新增的 VSUSP → Signo::SIGTSTP 映射）
     */
    if (write(master, "\x1a", 1) != 1) {
        printf("FAIL: write \\x1a to master: %s\n", strerror(errno));
        kill(child, SIGKILL); waitpid(child, NULL, 0);
        close(master);
        return 1;
    }
    close(master);

    int status = 0;
    if (waitpid(child, &status, 0) != child) {
        printf("FAIL: waitpid: %s\n", strerror(errno));
        return 1;
    }

    if (WIFEXITED(status) && WEXITSTATUS(status) == 0) {
        printf("PASS: PTY 行规程正确将 \\x1a 转换为 SIGTSTP 并投递到前台进程\n");
        return 0;
    }
    if (WIFEXITED(status))
        printf("FAIL: 子进程退出 code=%d（未收到 SIGTSTP 或信号映射缺失）\n",
               WEXITSTATUS(status));
    else
        printf("FAIL: 子进程被信号 %d 终止，期望 SIGTSTP 由自定义处理函数捕获\n",
               WTERMSIG(status));
    return 1;
}

int main(void)
{
    printf("=== bug-tty-sigint ===\n");
    printf("通过 PTY 验证 TTY 行规程控制字符 → 信号路径\n\n");

    int r1 = test_pty_ctrl_c();
    int r2 = test_pty_ctrl_z();

    printf("\n");
    if (r1 == 0 && r2 == 0) {
        printf("TEST PASSED\n");
        return 0;
    }
    printf("TEST FAILED\n");
    return 1;
}
