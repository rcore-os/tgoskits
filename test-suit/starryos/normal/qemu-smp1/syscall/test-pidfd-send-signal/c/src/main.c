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

#ifndef SI_USER
#define SI_USER 0
#endif
#ifndef SI_TKILL
#define SI_TKILL -6
#endif

static volatile int g_usr1_count;
static volatile siginfo_t g_last_si;

static int x_pidfd_open(pid_t pid, unsigned int flags)
{
    return (int)syscall(__NR_pidfd_open, pid, flags);
}

static int x_pidfd_send_signal(int pidfd, int sig, void *info, unsigned int flags)
{
    return (int)syscall(__NR_pidfd_send_signal, pidfd, sig, info, flags);
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

static void test_send_signal_default_self(void)
{
    printf("--- pidfd_send_signal 默认 SIGUSR1 ---\n");

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
}

static void test_send_signal_null_info_fills_pid(void)
{
    printf("--- pidfd_send_signal info=NULL si_pid ---\n");

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

    for (int i = 0; i < 1000 && kill(child, 0) != 0; i++) {
        usleep(1000);
    }
    CHECK(kill(child, 0) == 0, "子进程 zombie 未 reap");

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

    close(sync.notify_pipe[0]);
    close(sync.notify_pipe[1]);
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

static void test_send_signal_process_group(void)
{
    printf("--- pidfd_send_signal PROCESS_GROUP ---\n");

    int pfd = x_pidfd_open(getpid(), 0);
    CHECK(pfd >= 0, "pidfd_open 成功");
    if (pfd >= 0) {
        int r = x_pidfd_send_signal(pfd, 0, NULL, PIDFD_SIGNAL_PROCESS_GROUP);
        if (r == 0) {
            CHECK(1, "PROCESS_GROUP signo=0 探活成功");
        } else if (r == -1 && errno == EOPNOTSUPP) {
            CHECK(1, "PROCESS_GROUP 未实现 -> EOPNOTSUPP（可接受）");
        } else {
            CHECK(0, "PROCESS_GROUP 意外返回值");
        }
        close(pfd);
    }
}

int main(void)
{
    TEST_START("pidfd_send_signal");

    signal(SIGPIPE, SIG_IGN);

    test_send_signal_default_self();
    test_send_signal_null_info_fills_pid();
    test_send_signal_zero_probes();
    test_send_signal_sig_mismatch();
    test_send_signal_flag_thread_with_thread_pidfd();
    test_send_signal_flag_multi();
    test_send_signal_flag_unknown();
    test_send_signal_process_group();

    TEST_DONE();
}
