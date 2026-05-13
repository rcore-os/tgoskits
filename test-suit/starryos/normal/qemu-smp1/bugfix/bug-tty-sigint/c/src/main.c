/*
 * bug-tty-sigint: TTY 行规程控制字符路径回归测试。
 *
 * 覆盖三个独立的修复点：
 *
 * 测试 1/2 — PTY 路径（验证 ldisc 信号投递 + termios.rs 的 VSUSP 映射）：
 *   write(master_fd, "\x03"/"\x1a", 1)
 *     → PtyWriter 写入 master_to_slave 缓冲区，唤醒 poll_rx_slave
 *     → 从设备 InterruptDriven reader 任务唤醒
 *     → drain_source_into_line_buffer() 读取控制字符
 *     → check_send_signal() → signo_for() → SIGINT / SIGTSTP
 *     → send_signal_to_process_group(pgid, ...)
 *
 *   注：这两条用例走 pty.rs 中已有的 InterruptDriven 路径，
 *   不直接覆盖 ntty.rs 的 console_polling_mode() fallback，
 *   但确保 ldisc 信号投递逻辑（两条路径共用）和 VSUSP 映射正确。
 *
 * 测试 3 — N_TTY console 路径（验证 console_polling_mode + TIOCSTI）：
 *   通过 /dev/tty（N_TTY）使用 TIOCSTI 注入 0x03，
 *   验证后台 polling reader 任务将其经行规程转换为 SIGINT 并投递到
 *   前台进程组（即本进程自身）。
 *   若 console_polling_mode() 未生效（无后台 reader 任务），
 *   TIOCSTI 注入的字节将永远不会被处理，测试将超时失败。
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
    printf("[test1] PTY ldisc: write(master, \\x03) 应通过行规程发送 SIGINT\n");

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
    printf("[test2] PTY ldisc: write(master, \\x1a) 应通过行规程发送 SIGTSTP\n");

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
         * 验证信号确实经行规程投递（覆盖 termios.rs 新增的 VSUSP 映射）。
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
        printf("FAIL: 子进程退出 code=%d（未收到 SIGTSTP 或 VSUSP 映射缺失）\n",
               WEXITSTATUS(status));
    else
        printf("FAIL: 子进程被信号 %d 终止，期望 SIGTSTP 由自定义处理函数捕获\n",
               WTERMSIG(status));
    return 1;
}

/*
 * ---- 测试三：N_TTY console 路径 ——————————————————————————————————————
 *
 * 通过 /dev/tty（N_TTY 设备）使用 TIOCSTI 向 console 行规程注入 0x03，
 * 验证后台 console_polling_mode() reader 任务将其转换为 SIGINT 并投递到
 * 前台进程组（本进程）。
 *
 * 若 ntty.rs 的 console_polling_mode() 被回退（平台无 console IRQ 时无
 * 后台 reader 任务），TIOCSTI 注入的字节永远不会被 drain_source_into_line_buffer()
 * 消费，SIGINT 不会到来，本测试将超时后以 FAIL 返回。
 */
#ifndef TIOCSTI
#define TIOCSTI 0x5412
#endif

static volatile sig_atomic_t got_sigint = 0;
static void catch_sigint(int s) { (void)s; got_sigint = 1; }

static int test_console_ntty_ctrl_c(void)
{
    printf("[test3] N_TTY console: TIOCSTI(\\x03) 应经行规程投递 SIGINT\n");

    int tty = open("/dev/tty", O_RDWR | O_NOCTTY);
    if (tty < 0) {
        printf("[test3] SKIP: open /dev/tty: %s\n", strerror(errno));
        return 0;
    }

    /*
     * 确保本进程组是 N_TTY 的前台进程组，以便信号投递到此处。
     * TIOCSPGRP 失败时继续（可能已经是前台进程组）。
     */
    pid_t mypgid = getpgrp();
    ioctl(tty, TIOCSPGRP, &mypgid);

    struct sigaction sa_new = {0}, sa_old;
    sa_new.sa_handler = catch_sigint;
    sigemptyset(&sa_new.sa_mask);
    sigaction(SIGINT, &sa_new, &sa_old);
    got_sigint = 0;

    /*
     * 向 N_TTY 输入队列注入 Ctrl+C (0x03)。
     * TIOCSTI → ldisc.tiocsti() → tiocsti_byte 标志位置位 + pump_retry 唤醒
     *   → 后台 InterruptDriven reader 任务唤醒
     *   → drain_source_into_line_buffer() 取出注入字节
     *   → check_send_signal() → SIGINT → 前台进程组收到信号
     */
    char ctrl_c = 0x03;
    if (ioctl(tty, TIOCSTI, &ctrl_c) != 0) {
        printf("[test3] FAIL: TIOCSTI: %s\n", strerror(errno));
        sigaction(SIGINT, &sa_old, NULL);
        close(tty);
        return 1;
    }

    /* 等待后台 reader 任务（最多 1ms polling 周期 + 调度延迟）处理注入字节 */
    sleep_ms(200);

    sigaction(SIGINT, &sa_old, NULL);
    close(tty);

    if (got_sigint) {
        printf("PASS: N_TTY 行规程通过 TIOCSTI 正确投递 SIGINT（console_polling_mode 生效）\n");
        return 0;
    }
    printf("FAIL: N_TTY 行规程未投递 SIGINT（console_polling_mode 可能未生效）\n");
    return 1;
}

int main(void)
{
    printf("=== bug-tty-sigint ===\n");
    printf("验证 TTY 行规程控制字符信号投递（PTY 路径 + N_TTY console 路径）\n\n");

    int r1 = test_pty_ctrl_c();
    int r2 = test_pty_ctrl_z();
    int r3 = test_console_ntty_ctrl_c();

    printf("\n");
    if (r1 == 0 && r2 == 0 && r3 == 0) {
        printf("TEST PASSED\n");
        return 0;
    }
    printf("TEST FAILED\n");
    return 1;
}
