#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <string.h>
#include <sys/epoll.h>
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

static int epoll_pidfd(int pidfd, unsigned int interests,
                       unsigned int *ready_events)
{
    int epfd = epoll_create1(EPOLL_CLOEXEC);
    if (epfd < 0) {
        return -1;
    }

    struct epoll_event event = {
        .events = interests,
        .data.fd = pidfd,
    };
    if (epoll_ctl(epfd, EPOLL_CTL_ADD, pidfd, &event) != 0) {
        close(epfd);
        return -1;
    }

    int ready = epoll_wait(epfd, &event, 1, 0);
    if (ready == 1) {
        *ready_events = event.events;
    }
    close(epfd);
    return ready;
}

struct pidfd_hup_wait {
    int pidfd;
    int ready_pipe[2];
    int result;
    unsigned int ready_events;
};

static void *wait_for_pidfd_hup(void *arg)
{
    struct pidfd_hup_wait *wait = arg;
    int epfd = epoll_create1(EPOLL_CLOEXEC);
    if (epfd < 0) {
        wait->result = -1;
        return NULL;
    }

    struct epoll_event event = {
        .events = EPOLLHUP,
        .data.fd = wait->pidfd,
    };
    if (epoll_ctl(epfd, EPOLL_CTL_ADD, wait->pidfd, &event) != 0) {
        close(epfd);
        wait->result = -1;
        return NULL;
    }

    if (write(wait->ready_pipe[1], "x", 1) != 1) {
        close(epfd);
        wait->result = -1;
        return NULL;
    }

    wait->result = epoll_wait(epfd, &event, 1, 2000);
    if (wait->result == 1) {
        wait->ready_events = event.events;
    }
    close(epfd);
    return NULL;
}

static int wait_for_zombie_without_reaping(pid_t child, int expected_status)
{
    siginfo_t info;

    for (int i = 0; i < 3000; i++) {
        memset(&info, 0, sizeof(info));
        if (syscall(SYS_waitid, P_PID, child, &info,
                    WEXITED | WNOWAIT | WNOHANG, NULL) != 0) {
            return -1;
        }
        if (info.si_pid == child) {
            if (info.si_code != CLD_EXITED || info.si_status != expected_status) {
                errno = EPROTO;
                return -1;
            }
            return 0;
        }
        usleep(1000);
    }

    errno = ETIMEDOUT;
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

static void test_send_signal_zombie_identity(void)
{
    printf("--- pidfd_send_signal zombie identity 生命周期 ---\n");

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
        _exit(23);
    }

    int zombie_wait = wait_for_zombie_without_reaping(child, 23);
    CHECK_RET(zombie_wait, 0, "waitid(WNOWAIT) 确认 zombie 且不 reap");
    if (zombie_wait != 0) {
        waitpid(child, NULL, 0);
        return;
    }

    pfd = x_pidfd_open(child, 0);
    CHECK(pfd >= 0, "未 reap zombie 的 pidfd_open 成功");
    if (pfd < 0) {
        waitpid(child, NULL, 0);
        return;
    }

    struct pollfd pollfd = {
        .fd = pfd,
        .events = POLLIN | POLLRDNORM | POLLHUP,
    };
    CHECK_RET(poll(&pollfd, 1, 0), 1, "未 reap zombie 的 pidfd 已就绪");
    CHECK((pollfd.revents & (POLLIN | POLLRDNORM)) ==
              (POLLIN | POLLRDNORM),
          "未 reap zombie 返回 POLLIN|POLLRDNORM");
    CHECK((pollfd.revents & POLLHUP) == 0, "未 reap zombie 不返回 POLLHUP");
    unsigned int epoll_events = 0;
    CHECK_RET(epoll_pidfd(pfd, EPOLLRDNORM, &epoll_events), 1,
              "EPOLLRDNORM 单独监听未 reap zombie");
    CHECK((epoll_events & EPOLLRDNORM) != 0,
          "未 reap zombie 的 epoll 结果包含 EPOLLRDNORM");

    CHECK_RET(x_pidfd_send_signal(pfd, 0, NULL, 0), 0,
              "未 reap zombie 的 signo=0 探活成功");
    CHECK_RET(x_pidfd_send_signal(pfd, SIGUSR1, NULL, 0), 0,
              "未 reap zombie 接受非零信号并保持退出状态");
    CHECK_RET(wait_for_zombie_without_reaping(child, 23), 0,
              "非零信号不改变 zombie 的原退出状态");

    struct pidfd_hup_wait hup_wait = {
        .pidfd = pfd,
        .result = -1,
        .ready_events = 0,
    };
    int hup_pipe = pipe(hup_wait.ready_pipe);
    CHECK_RET(hup_pipe, 0, "创建 pidfd reap 通知同步管道");
    pthread_t hup_thread;
    int hup_thread_started = -1;
    if (hup_pipe == 0) {
        hup_thread_started =
            pthread_create(&hup_thread, NULL, wait_for_pidfd_hup, &hup_wait);
        CHECK_RET(hup_thread_started, 0, "启动 EPOLLHUP 等待线程");
        if (hup_thread_started == 0) {
            char ready;
            CHECK_RET(read(hup_wait.ready_pipe[0], &ready, 1), 1,
                      "EPOLLHUP waiter 已完成注册");
        }
    }

    int status = 0;
    CHECK_RET(waitpid(child, &status, 0), child, "waitpid 唯一 reap zombie");
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 23, "reap 返回原退出状态");
    if (hup_thread_started == 0) {
        CHECK_RET(pthread_join(hup_thread, NULL), 0, "等待 EPOLLHUP waiter 完成");
        CHECK_RET(hup_wait.result, 1, "reap 唤醒已有 pidfd 的 EPOLLHUP waiter");
        CHECK((hup_wait.ready_events & EPOLLHUP) != 0,
              "reap 唤醒结果包含 EPOLLHUP");
    }
    if (hup_pipe == 0) {
        close(hup_wait.ready_pipe[0]);
        close(hup_wait.ready_pipe[1]);
    }
    pollfd.revents = 0;
    CHECK_RET(poll(&pollfd, 1, 0), 1, "reap 后已有 pidfd 仍为终止态");
    CHECK((pollfd.revents & (POLLIN | POLLRDNORM | POLLHUP)) ==
              (POLLIN | POLLRDNORM | POLLHUP),
          "reap 后已有 pidfd 返回 POLLIN|POLLRDNORM|POLLHUP");
    epoll_events = 0;
    CHECK_RET(epoll_pidfd(pfd, EPOLLRDNORM, &epoll_events), 1,
              "EPOLLRDNORM 单独监听 reap 后 pidfd");
    CHECK((epoll_events & (EPOLLRDNORM | EPOLLHUP)) ==
              (EPOLLRDNORM | EPOLLHUP),
          "reap 后 pidfd 的 epoll 结果包含 EPOLLRDNORM|EPOLLHUP");
    CHECK_ERR(x_pidfd_send_signal(pfd, 0, NULL, 0), ESRCH,
              "已有 pidfd 在 reap 后 signo=0 -> ESRCH");
    CHECK_ERR(x_pidfd_send_signal(pfd, SIGUSR1, NULL, 0), ESRCH,
              "已有 pidfd 在 reap 后 SIGUSR1 -> ESRCH");
    close(pfd);

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
    test_send_signal_zombie_identity();
    test_send_signal_flag_thread_with_thread_pidfd();
    test_send_signal_thread_pidfd_thread_group_flag();

    TEST_DONE();
}
