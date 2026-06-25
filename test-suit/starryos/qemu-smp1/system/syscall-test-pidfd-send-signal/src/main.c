#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef __NR_pidfd_open
#error "__NR_pidfd_open required"
#endif
#ifndef __NR_pidfd_send_signal
#error "__NR_pidfd_send_signal required"
#endif

#ifndef PIDFD_THREAD
#define PIDFD_THREAD O_EXCL
#endif
#ifndef PIDFD_SIGNAL_THREAD
#define PIDFD_SIGNAL_THREAD (1U << 0)
#define PIDFD_SIGNAL_THREAD_GROUP (1U << 1)
#define PIDFD_SIGNAL_PROCESS_GROUP (1U << 2)
#endif

#ifndef WNOWAIT
#define WNOWAIT 0x01000000
#endif

#ifndef SI_USER
#define SI_USER 0
#endif

static volatile int g_usr1_count;
static volatile siginfo_t g_last_si;
static sigset_t g_usr1_mask;

static void block_usr1(void)
{
    sigemptyset(&g_usr1_mask);
    sigaddset(&g_usr1_mask, SIGUSR1);
    pthread_sigmask(SIG_BLOCK, &g_usr1_mask, NULL);
}

static void unblock_usr1(void)
{
    pthread_sigmask(SIG_UNBLOCK, &g_usr1_mask, NULL);
}

static int x_pidfd_open(pid_t pid, unsigned int flags)
{
    return (int)syscall(__NR_pidfd_open, pid, flags);
}

static int x_pidfd_send_signal(int pidfd, int sig, void *info, unsigned int flags)
{
    return (int)syscall(__NR_pidfd_send_signal, pidfd, sig, info, flags);
}

static int wait_child_zombie(pid_t child)
{
    siginfo_t info;

    for (int i = 0; i < 1000; i++) {
        memset(&info, 0, sizeof(info));
        if (waitid(P_PID, (id_t)child, &info, WEXITED | WNOWAIT | WNOHANG) == 0 &&
            info.si_pid == child) {
            return 0;
        }
        usleep(1000);
    }
    return -1;
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

static void usr1_handler(int signo)
{
    (void)signo;
    g_usr1_count++;
}

static void usr1_sigaction_handler(int signo, siginfo_t *si, void *ctx)
{
    (void)signo;
    (void)ctx;
    if (si) {
        g_last_si = *si;
    }
    g_usr1_count++;
}

static void test_send_signal_bad_pidfd(void)
{
    printf("--- pidfd_send_signal 无效 pidfd ---\n");

    CHECK_ERR(x_pidfd_send_signal(-1, SIGUSR1, NULL, 0), EBADF, "pidfd=-1 -> EBADF");

    int pfd = x_pidfd_open(getpid(), 0);
    CHECK(pfd >= 0, "pidfd_open 成功");
    if (pfd >= 0) {
        close(pfd);
        CHECK_ERR(x_pidfd_send_signal(pfd, SIGUSR1, NULL, 0), EBADF,
                  "已 close pidfd -> EBADF");
    }

    int pipe_fds[2];
    CHECK_RET(pipe(pipe_fds), 0, "pipe 创建成功");
    errno = 0;
    if (x_pidfd_send_signal(pipe_fds[0], SIGUSR1, NULL, 0) == -1 &&
        (errno == EINVAL || errno == EBADF)) {
        CHECK(1, "普通 fd 作 pidfd -> EINVAL/EBADF");
    } else {
        CHECK(0, "普通 fd 作 pidfd -> EINVAL/EBADF");
    }
    close(pipe_fds[0]);
    close(pipe_fds[1]);
}

static void test_send_signal_reaped_target(void)
{
    printf("--- pidfd_send_signal reap 后目标进程 ---\n");

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

    CHECK_ERR(x_pidfd_send_signal(pfd, SIGUSR1, NULL, 0), ESRCH,
              "reap 后 SIGUSR1 -> ESRCH");
    CHECK_ERR(x_pidfd_send_signal(pfd, 0, NULL, 0), ESRCH, "reap 后 signo=0 -> ESRCH");
    close(pfd);
    close(sync[1]);
}

static void test_send_signal_invalid_signo(void)
{
    printf("--- pidfd_send_signal 非法 signo ---\n");

    int pfd = x_pidfd_open(getpid(), 0);
    CHECK(pfd >= 0, "pidfd_open 成功");
    if (pfd >= 0) {
        CHECK_ERR(x_pidfd_send_signal(pfd, -1, NULL, 0), EINVAL, "signo=-1 -> EINVAL");
        CHECK_ERR(x_pidfd_send_signal(pfd, 999, NULL, 0), EINVAL, "signo=999 -> EINVAL");
        close(pfd);
    }
}

static void test_send_signal_bad_info_pointer(void)
{
    printf("--- pidfd_send_signal info 非法指针 ---\n");

    int pfd = x_pidfd_open(getpid(), 0);
    CHECK(pfd >= 0, "pidfd_open 成功");
    if (pfd >= 0) {
        CHECK_ERR(x_pidfd_send_signal(pfd, SIGUSR1, (void *)1, 0), EFAULT,
                  "info=(void*)1 -> EFAULT");
        close(pfd);
    }
}

static void test_send_signal_sig_mismatch(void)
{
    printf("--- pidfd_send_signal sig 与 info 不一致 ---\n");

    int pfd = x_pidfd_open(getpid(), 0);
    CHECK(pfd >= 0, "pidfd_open 成功");
    if (pfd < 0) {
        return;
    }

    siginfo_t info;
    memset(&info, 0, sizeof(info));
    info.si_signo = SIGUSR2;

    CHECK_ERR(x_pidfd_send_signal(pfd, SIGUSR1, &info, 0), EINVAL,
              "sig != info.si_signo -> EINVAL");
    close(pfd);
}

static void test_send_signal_flag_multi(void)
{
    printf("--- pidfd_send_signal 多个 scope flags ---\n");

    int pfd = x_pidfd_open(getpid(), 0);
    CHECK(pfd >= 0, "pidfd_open 成功");
    if (pfd >= 0) {
        unsigned int flags = PIDFD_SIGNAL_THREAD | PIDFD_SIGNAL_THREAD_GROUP;
        CHECK_ERR(x_pidfd_send_signal(pfd, SIGUSR1, NULL, flags), EINVAL,
                  "两个 scope flags -> EINVAL");
        close(pfd);
    }
}

static void test_send_signal_flag_unknown(void)
{
    printf("--- pidfd_send_signal 未知 flags ---\n");

    int pfd = x_pidfd_open(getpid(), 0);
    CHECK(pfd >= 0, "pidfd_open 成功");
    if (pfd >= 0) {
        CHECK_ERR(x_pidfd_send_signal(pfd, SIGUSR1, NULL, 0x10000u), EINVAL,
                  "未知 flags -> EINVAL");
        close(pfd);
    }
}

static void test_send_signal_tgid_with_thread_flag(void)
{
    printf("--- tgid pidfd + PIDFD_SIGNAL_THREAD ---\n");

    int pfd = x_pidfd_open(getpid(), 0);
    CHECK(pfd >= 0, "pidfd_open 成功");
    if (pfd >= 0) {
        CHECK_ERR(x_pidfd_send_signal(pfd, SIGUSR1, NULL, PIDFD_SIGNAL_THREAD), EINVAL,
                  "tgid pidfd + THREAD flag -> EINVAL");
        close(pfd);
    }
}

static void test_send_signal_process_group(void)
{
    printf("--- pidfd_send_signal PROCESS_GROUP ---\n");

    int pfd = x_pidfd_open(getpid(), 0);
    CHECK(pfd >= 0, "pidfd_open 成功");
    if (pfd >= 0) {
        CHECK_RET(x_pidfd_send_signal(pfd, 0, NULL, PIDFD_SIGNAL_PROCESS_GROUP), 0,
                  "PROCESS_GROUP signo=0 探活成功");
        close(pfd);
    }
}

static void test_send_signal_valid_info(void)
{
    printf("--- pidfd_send_signal 有效 info ---\n");

    unblock_usr1();
    g_usr1_count = 0;
    struct sigaction sa = {0};
    sa.sa_sigaction = usr1_sigaction_handler;
    sa.sa_flags = SA_SIGINFO;
    sigemptyset(&sa.sa_mask);
    CHECK_RET(sigaction(SIGUSR1, &sa, NULL), 0, "sigaction 安装");

    int pfd = x_pidfd_open(getpid(), 0);
    CHECK(pfd >= 0, "pidfd_open 成功");
    if (pfd < 0) {
        return;
    }

    siginfo_t info;
    memset(&info, 0, sizeof(info));
    info.si_signo = SIGUSR1;

    CHECK_RET(x_pidfd_send_signal(pfd, SIGUSR1, &info, 0), 0, "send_signal 带 info 成功");
    usleep(100000);
    CHECK(g_usr1_count >= 1, "handler 被调用");
    close(pfd);
    block_usr1();
}

static void test_send_signal_default_self(void)
{
    printf("--- pidfd_send_signal 默认 SIGUSR1 ---\n");

    unblock_usr1();
    g_usr1_count = 0;
    signal(SIGUSR1, usr1_handler);

    int pfd = x_pidfd_open(getpid(), 0);
    CHECK(pfd >= 0, "pidfd_open 成功");
    if (pfd < 0) {
        return;
    }

    CHECK_RET(x_pidfd_send_signal(pfd, SIGUSR1, NULL, 0), 0, "pidfd_send_signal 成功");
    usleep(100000);
    CHECK(g_usr1_count == 1, "SIGUSR1 handler 被调用一次");
    close(pfd);
    block_usr1();
}

static void test_send_signal_null_info_fills_pid(void)
{
    printf("--- pidfd_send_signal info=NULL si_pid ---\n");

    unblock_usr1();
    g_usr1_count = 0;
    struct sigaction sa = {0};
    sa.sa_sigaction = usr1_sigaction_handler;
    sa.sa_flags = SA_SIGINFO;
    sigemptyset(&sa.sa_mask);
    CHECK_RET(sigaction(SIGUSR1, &sa, NULL), 0, "sigaction 安装");

    int pfd = x_pidfd_open(getpid(), 0);
    CHECK(pfd >= 0, "pidfd_open 成功");
    if (pfd < 0) {
        return;
    }

    CHECK_RET(x_pidfd_send_signal(pfd, SIGUSR1, NULL, 0), 0, "send_signal 成功");
    usleep(100000);
    CHECK(g_usr1_count >= 1, "handler 被调用");
    CHECK((int)g_last_si.si_pid == (int)getpid(), "si_pid == getpid()");
    CHECK(g_last_si.si_code == SI_USER, "si_code == SI_USER");
    close(pfd);
    block_usr1();
}

static void test_send_signal_zero_probes(void)
{
    printf("--- pidfd_send_signal signo=0 探活 ---\n");

    int pfd = x_pidfd_open(getpid(), 0);
    CHECK(pfd >= 0, "pidfd_open 成功");
    if (pfd >= 0) {
        CHECK_RET(x_pidfd_send_signal(pfd, 0, NULL, 0), 0, "存活进程 signo=0 探活");
        close(pfd);
    }

    pid_t child = fork();
    CHECK(child >= 0, "fork 成功");
    if (child < 0) {
        return;
    }
    if (child == 0) {
        _exit(0);
    }

    CHECK(wait_child_zombie(child) == 0, "子进程 zombie 未 reap");

    pfd = x_pidfd_open(child, 0);
    if (pfd >= 0) {
        CHECK_RET(x_pidfd_send_signal(pfd, 0, NULL, 0), 0, "zombie signo=0 探活");
        close(pfd);
    }

    int status = 0;
    waitpid(child, &status, 0);
    errno = 0;
    pfd = x_pidfd_open(child, 0);
    CHECK(pfd == -1 && errno == ESRCH, "reap 后 pidfd_open -> ESRCH");
}

struct thread_tid_sync {
    int notify_pipe[2];
    pid_t tid;
    volatile int thread_got_usr1;
};

static struct thread_tid_sync *g_thread_sync;

static void thread_usr1_handler(int signo)
{
    (void)signo;
    if (g_thread_sync) {
        g_thread_sync->thread_got_usr1 = 1;
    }
}

static void *thread_wait_usr1(void *arg)
{
    struct thread_tid_sync *sync = arg;

    g_thread_sync = sync;
    unblock_usr1();
    signal(SIGUSR1, thread_usr1_handler);
    sync->tid = (pid_t)syscall(SYS_gettid);
    if (write(sync->notify_pipe[1], "x", 1) != 1) {
        return (void *)1;
    }

    for (int i = 0; i < 3000 && !sync->thread_got_usr1; i++) {
        usleep(10000);
    }
    g_thread_sync = NULL;
    return NULL;
}

static void test_send_signal_flag_thread_with_thread_pidfd(void)
{
    printf("--- PIDFD_THREAD pidfd + PIDFD_SIGNAL_THREAD ---\n");

    struct thread_tid_sync sync = { .tid = -1, .thread_got_usr1 = 0 };
    pthread_t thread;

    if (pipe(sync.notify_pipe) != 0) {
        return;
    }

    g_usr1_count = 0;
    signal(SIGUSR1, SIG_IGN);
    unblock_usr1();

    CHECK(pthread_create(&thread, NULL, thread_wait_usr1, &sync) == 0,
          "pthread_create 成功");

    char ch;
    CHECK(read(sync.notify_pipe[0], &ch, 1) == 1, "等待子线程 tid");

    int pfd = x_pidfd_open(sync.tid, PIDFD_THREAD);
    CHECK(pfd >= 0, "pidfd_open(tid, PIDFD_THREAD) 成功");
    if (pfd >= 0) {
        CHECK_RET(x_pidfd_send_signal(pfd, SIGUSR1, NULL, PIDFD_SIGNAL_THREAD),
                  0, "向线程发 SIGUSR1");
        for (int i = 0; i < 3000 && !sync.thread_got_usr1; i++) {
            usleep(10000);
        }
        CHECK(sync.thread_got_usr1 == 1, "子线程收到 SIGUSR1");
        CHECK(g_usr1_count == 0, "主线程 SIG_IGN 未收到 SIGUSR1");
        pthread_join(thread, NULL);
        close(pfd);
    }

    block_usr1();
    close(sync.notify_pipe[0]);
    close(sync.notify_pipe[1]);
}

/*
 * THREAD_GROUP on a thread-level pidfd selects ThreadGroup scope: signal goes
 * to the whole thread group. Main thread SIG_IGN; worker thread should receive.
 */
static void test_send_signal_thread_pidfd_thread_group_flag(void)
{
    printf("--- thread pidfd + PIDFD_SIGNAL_THREAD_GROUP ---\n");

    struct thread_tid_sync sync = { .tid = -1, .thread_got_usr1 = 0 };
    pthread_t thread;

    if (pipe(sync.notify_pipe) != 0) {
        return;
    }

    g_usr1_count = 0;
    signal(SIGUSR1, SIG_IGN);
    unblock_usr1();

    CHECK(pthread_create(&thread, NULL, thread_wait_usr1, &sync) == 0,
          "pthread_create 成功");

    char ch;
    CHECK(read(sync.notify_pipe[0], &ch, 1) == 1, "等待子线程 tid");

    int pfd = x_pidfd_open(sync.tid, PIDFD_THREAD);
    CHECK(pfd >= 0, "pidfd_open(tid, PIDFD_THREAD) 成功");
    if (pfd >= 0) {
        CHECK_RET(x_pidfd_send_signal(pfd, SIGUSR1, NULL, PIDFD_SIGNAL_THREAD_GROUP),
                  0, "THREAD_GROUP 向线程组发 SIGUSR1");
        for (int i = 0; i < 3000 && !sync.thread_got_usr1; i++) {
            usleep(10000);
        }
        CHECK(sync.thread_got_usr1 == 1, "子线程收到 SIGUSR1");
        CHECK(g_usr1_count == 0, "主线程 SIG_IGN 未收到 SIGUSR1");
        pthread_join(thread, NULL);
        close(pfd);
    }

    block_usr1();
    close(sync.notify_pipe[0]);
    close(sync.notify_pipe[1]);
}

int main(void)
{
    TEST_START("pidfd_send_signal");

    signal(SIGPIPE, SIG_IGN);
    signal(SIGUSR1, SIG_IGN);
    block_usr1();

    test_send_signal_bad_pidfd();
    test_send_signal_reaped_target();
    test_send_signal_invalid_signo();
    test_send_signal_bad_info_pointer();
    test_send_signal_sig_mismatch();
    test_send_signal_flag_multi();
    test_send_signal_flag_unknown();
    test_send_signal_tgid_with_thread_flag();
    test_send_signal_process_group();
    test_send_signal_valid_info();
    test_send_signal_default_self();
    test_send_signal_null_info_fills_pid();
    test_send_signal_zero_probes();
    test_send_signal_flag_thread_with_thread_pidfd();
    test_send_signal_thread_pidfd_thread_group_flag();

    TEST_DONE();
}
