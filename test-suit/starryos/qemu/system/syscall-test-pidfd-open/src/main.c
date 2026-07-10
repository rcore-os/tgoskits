#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef __NR_pidfd_open
#error "__NR_pidfd_open required from <sys/syscall.h> for this arch/toolchain"
#endif

#ifndef PIDFD_THREAD
#define PIDFD_THREAD O_EXCL
#endif

#ifndef PIDFD_NONBLOCK
#define PIDFD_NONBLOCK O_NONBLOCK
#endif

static int x_pidfd_open(pid_t pid, unsigned int flags)
{
    return (int)syscall(__NR_pidfd_open, pid, flags);
}

static int get_cloexec(int fd)
{
    int flags = fcntl(fd, F_GETFD);
    if (flags == -1) {
        return -1;
    }
    return !!(flags & FD_CLOEXEC);
}

static void test_pidfd_open_invalid_pid(void)
{
    printf("--- pidfd_open 非法 pid ---\n");

    CHECK_ERR(x_pidfd_open(-1, 0), EINVAL, "pidfd_open(-1, 0) -> EINVAL");
    CHECK_ERR(x_pidfd_open(0, 0), EINVAL, "pidfd_open(0, 0) -> EINVAL");
}

static void test_pidfd_open_self(void)
{
    printf("--- pidfd_open 正常路径 ---\n");

    errno = 0;
    int pfd = x_pidfd_open(getpid(), 0);
    CHECK(pfd >= 0, "pidfd_open(getpid(), 0) 返回 fd");
    if (pfd >= 0) {
        CHECK_RET(close(pfd), 0, "close pidfd");
    }
}

static void test_pidfd_open_cloexec(void)
{
    printf("--- pidfd_open O_CLOEXEC ---\n");

    int pfd = x_pidfd_open(getpid(), 0);
    CHECK(pfd >= 0, "pidfd_open(getpid(), 0) 成功");
    if (pfd >= 0) {
        CHECK(get_cloexec(pfd) == 1, "pidfd 带 FD_CLOEXEC");
        close(pfd);
    }
}

static void test_pidfd_open_stale(void)
{
    printf("--- pidfd_open 不存在 pid ---\n");

    errno = 0;
    pid_t stale = (pid_t)999999001;
    if (stale <= 0) {
        stale = (pid_t)2147483644;
    }
    CHECK_ERR(x_pidfd_open(stale, 0), ESRCH, "不存在 pid -> ESRCH");
}

static void test_pidfd_open_bad_flags(void)
{
    printf("--- pidfd_open 非法 flags ---\n");

    CHECK_ERR(x_pidfd_open(getpid(), 0xFFFFFFFFu), EINVAL, "非法 flags -> EINVAL");
    CHECK_ERR(x_pidfd_open(getpid(), 1u), EINVAL, "未知 flags 位 -> EINVAL");
}

struct thread_tid_sync {
    volatile pid_t tid;
};

static void *thread_publish_tid(void *arg)
{
    struct thread_tid_sync *sync = arg;

    sync->tid = (pid_t)syscall(SYS_gettid);
    return NULL;
}

static void test_pidfd_open_thread_tid(void)
{
    printf("--- pidfd_open 线程 TID ---\n");

    struct thread_tid_sync sync = { .tid = -1 };
    pthread_t thread;

    CHECK(pthread_create(&thread, NULL, thread_publish_tid, &sync) == 0,
          "pthread_create 成功");

    for (int i = 0; i < 1000000 && sync.tid <= 0; i++) {
        sched_yield();
    }
    CHECK(sync.tid > 0 && sync.tid != getpid(), "子线程 tid 与 getpid 不同");

    CHECK_ERR(x_pidfd_open(sync.tid, 0), ENOENT,
              "非 leader 线程 tid 无 PIDFD_THREAD -> ENOENT");

    int pfd = x_pidfd_open(sync.tid, PIDFD_THREAD);
    CHECK(pfd >= 0, "PIDFD_THREAD 打开子线程 tid 成功");
    if (pfd >= 0) {
        close(pfd);
    }

    pthread_join(thread, NULL);
}

static void test_pidfd_open_zombie(void)
{
    printf("--- pidfd_open zombie ---\n");

    pid_t child = fork();
    CHECK(child >= 0, "fork 成功");
    if (child < 0) {
        return;
    }

    if (child == 0) {
        _exit(0);
    }

    /* waitpid(WNOHANG) reaps the child; poll with kill(0) until it is a zombie. */
    for (int i = 0; i < 1000; i++) {
        if (kill(child, 0) == 0) {
            break;
        }
        usleep(1000);
    }
    CHECK(kill(child, 0) == 0, "子进程已退出且尚未 reap");

    int pfd = x_pidfd_open(child, 0);
    CHECK(pfd >= 0, "reap 前 pidfd_open(zombie child) 成功");
    if (pfd >= 0) {
        close(pfd);
    }

    int status = 0;
    CHECK_RET(waitpid(child, &status, 0), child, "waitpid reap 子进程");
    CHECK_ERR(x_pidfd_open(child, 0), ESRCH, "reap 后 pidfd_open(child) -> ESRCH");
}

int main(void)
{
    TEST_START("pidfd_open");

    signal(SIGPIPE, SIG_IGN);

    test_pidfd_open_invalid_pid();
    test_pidfd_open_self();
    test_pidfd_open_cloexec();
    test_pidfd_open_stale();
    test_pidfd_open_bad_flags();
    test_pidfd_open_thread_tid();
    test_pidfd_open_zombie();

    TEST_DONE();
}
