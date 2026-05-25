/*
 * epoll_wait2 语义测试
 */


#include "test_framework.h"
#include <errno.h>
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <sys/epoll.h>
#include <sys/syscall.h>
#include <time.h>
#include <sys/wait.h>
#include <unistd.h>

#define KERNEL_SIGSET_SIZE 8

static void on_sigusr1(int signo)
{
    (void)signo;
}

static long raw_epoll_pwait2(int epfd, struct epoll_event *events, int maxevents,
                             const struct timespec *timeout,
                             const sigset_t *sigmask, size_t sigsetsize)
{
    return syscall(SYS_epoll_pwait2, epfd, events, maxevents, timeout, sigmask, sigsetsize);
}

/* ERRORS: EBADF, EINVAL */
static void test_invalid_args(void)
{
    struct epoll_event out[4];

    errno = 0;
    CHECK(raw_epoll_pwait2(-1, out, 4, NULL, NULL, KERNEL_SIGSET_SIZE) == -1 && errno == EBADF,
          "invalid epfd returns EBADF");

    int epfd = epoll_create1(0);
    CHECK(epfd >= 0, "epoll_create1 ok");
    if (epfd < 0) {
        return;
    }

    errno = 0;
    CHECK(raw_epoll_pwait2(epfd, out, 0, NULL, NULL, KERNEL_SIGSET_SIZE) == -1 && errno == EINVAL,
          "maxevents == 0 returns EINVAL");

    errno = 0;
    CHECK(raw_epoll_pwait2(epfd, out, -1, NULL, NULL, KERNEL_SIGSET_SIZE) == -1 && errno == EINVAL,
          "maxevents < 0 returns EINVAL");

    close(epfd);
}

/* DESCRIPTION: timeout=0 立即返回; timeout=NULL 可无限等待 */
static void test_null_timeout_and_zero_timeout(void)
{
    int epfd = epoll_create1(0);
    CHECK(epfd >= 0, "epoll_create1 ok (timeout tests)");
    if (epfd < 0) {
        return;
    }

    int pipefd[2];
    int pipe_ret = pipe(pipefd);
    CHECK(pipe_ret == 0, "pipe created");
    if (pipe_ret != 0) {
        close(epfd);
        return;
    }

    struct epoll_event ev = {
        .events = EPOLLIN,
        .data = {.fd = pipefd[0]},
    };
    CHECK_RET(epoll_ctl(epfd, EPOLL_CTL_ADD, pipefd[0], &ev), 0, "epoll_ctl ADD read end");

    struct epoll_event out[4];
    struct timespec ts0 = {.tv_sec = 0, .tv_nsec = 0};

    CHECK_RET(raw_epoll_pwait2(epfd, out, 4, &ts0, NULL, KERNEL_SIGSET_SIZE), 0,
              "zero timeout returns 0 when no fd is ready");

    CHECK_RET(write(pipefd[1], "X", 1), 1, "write one byte to pipe");
    long n = raw_epoll_pwait2(epfd, out, 4, NULL, NULL, KERNEL_SIGSET_SIZE);
    CHECK(n == 1 && (out[0].events & EPOLLIN) != 0 && out[0].data.fd == pipefd[0],
          "NULL timeout waits and returns ready event");

    close(pipefd[0]);
    close(pipefd[1]);
    close(epfd);
}

/* C library/kernel differences + ERRORS: EINVAL */
static void test_timeout_and_sigsetsize(void)
{
    int epfd = epoll_create1(0);
    CHECK(epfd >= 0, "epoll_create1 ok (sigsetsize tests)");
    if (epfd < 0) {
        return;
    }

    struct epoll_event out[4];
    struct timespec ts = {.tv_sec = 0, .tv_nsec = 100 * 1000 * 1000};
    sigset_t mask;
    sigemptyset(&mask);

    CHECK_RET(raw_epoll_pwait2(epfd, out, 4, &ts, NULL, 0), 0,
              "NULL sigmask ignores sigsetsize and times out");

    CHECK_RET(raw_epoll_pwait2(epfd, out, 4, &ts, &mask, KERNEL_SIGSET_SIZE), 0,
              "sigmask + size=8 accepted");

    errno = 0;
    CHECK(raw_epoll_pwait2(epfd, out, 4, &ts, &mask, 4) == -1 && errno == EINVAL,
          "sigmask with too small sigsetsize returns EINVAL");

    errno = 0;
    size_t bad_size = sizeof(sigset_t) == KERNEL_SIGSET_SIZE ? 16 : sizeof(sigset_t);
    CHECK(raw_epoll_pwait2(epfd, out, 4, &ts, &mask, bad_size) == -1 && errno == EINVAL,
          "sigmask with non-8 sigsetsize returns EINVAL");

    close(epfd);
}

/* ERRORS: EFAULT（events 指针不可写） */
static void test_efault_events_ptr(void)
{
    int epfd = epoll_create1(0);
    CHECK(epfd >= 0, "epoll_create1 ok (EFAULT test)");
    if (epfd < 0) {
        return;
    }

    int pipefd[2];
    int pipe_ret = pipe(pipefd);
    CHECK(pipe_ret == 0, "pipe created (EFAULT test)");
    if (pipe_ret != 0) {
        close(epfd);
        return;
    }

    struct epoll_event ev = {
        .events = EPOLLIN,
        .data = {.fd = pipefd[0]},
    };
    CHECK_RET(epoll_ctl(epfd, EPOLL_CTL_ADD, pipefd[0], &ev), 0, "epoll_ctl ADD read end");
    CHECK_RET(write(pipefd[1], "Z", 1), 1, "write one byte to make event ready");

    errno = 0;
    long n = raw_epoll_pwait2(epfd, (struct epoll_event *)(uintptr_t)1, 1, NULL, NULL, KERNEL_SIGSET_SIZE);
    CHECK(n == -1 && errno == EFAULT, "invalid events pointer returns EFAULT");

    close(pipefd[0]);
    close(pipefd[1]);
    close(epfd);
}

/* DESCRIPTION: timeout 受 CLOCK_MONOTONIC 计时，允许小幅超调 */
static void test_timeout_monotonic_duration(void)
{
    int epfd = epoll_create1(0);
    CHECK(epfd >= 0, "epoll_create1 ok (timeout duration)");
    if (epfd < 0) {
        return;
    }

    struct epoll_event out[2];
    struct timespec ts = {.tv_sec = 0, .tv_nsec = 100 * 1000 * 1000};
    struct timespec start;
    struct timespec end;
    CHECK(clock_gettime(CLOCK_MONOTONIC, &start) == 0, "clock_gettime start ok");

    long n = raw_epoll_pwait2(epfd, out, 2, &ts, NULL, KERNEL_SIGSET_SIZE);
    CHECK(n == 0, "no event with 100ms timeout returns 0");

    CHECK(clock_gettime(CLOCK_MONOTONIC, &end) == 0, "clock_gettime end ok");

    long long elapsed_ms = (long long)(end.tv_sec - start.tv_sec) * 1000LL
                         + (long long)(end.tv_nsec - start.tv_nsec) / 1000000LL;
    CHECK(elapsed_ms >= 60 && elapsed_ms <= 1000,
          "timeout waits roughly expected duration (>=60ms)");

    close(epfd);
}

/* man page NOTES: 多于 maxevents 时可在后续调用中轮转返回 */
static void test_maxevents_boundary(void)
{
    int epfd = epoll_create1(0);
    CHECK(epfd >= 0, "epoll_create1 ok (maxevents boundary)");
    if (epfd < 0) {
        return;
    }

    int p1[2], p2[2];
    int p1_ret = pipe(p1);
    int p2_ret = pipe(p2);
    CHECK(p1_ret == 0, "pipe1 created");
    CHECK(p2_ret == 0, "pipe2 created");
    if (p1_ret != 0 || p2_ret != 0) {
        if (p1_ret == 0) {
            close(p1[0]);
            close(p1[1]);
        }
        if (p2_ret == 0) {
            close(p2[0]);
            close(p2[1]);
        }
        close(epfd);
        return;
    }

    struct epoll_event ev1 = {.events = EPOLLIN, .data = {.fd = p1[0]}};
    struct epoll_event ev2 = {.events = EPOLLIN, .data = {.fd = p2[0]}};
    CHECK_RET(epoll_ctl(epfd, EPOLL_CTL_ADD, p1[0], &ev1), 0, "add pipe1 read end");
    CHECK_RET(epoll_ctl(epfd, EPOLL_CTL_ADD, p2[0], &ev2), 0, "add pipe2 read end");
    CHECK_RET(write(p1[1], "A", 1), 1, "write pipe1");
    CHECK_RET(write(p2[1], "B", 1), 1, "write pipe2");

    struct epoll_event one[1];
    long n1 = raw_epoll_pwait2(epfd, one, 1, NULL, NULL, KERNEL_SIGSET_SIZE);
    CHECK(n1 == 1, "maxevents=1 returns exactly one ready event");

    struct epoll_event many[1024];
    long n2 = raw_epoll_pwait2(epfd, many, 1024, NULL, NULL, KERNEL_SIGSET_SIZE);
    CHECK(n2 >= 1, "large maxevents can fetch remaining ready events");

    close(p1[0]);
    close(p1[1]);
    close(p2[0]);
    close(p2[1]);
    close(epfd);
}

/* epoll_pwait()/epoll_pwait2: 临时信号掩码语义 + EINTR */
static void test_sigmask_effect_and_eintr(void)
{
    struct sigaction sa;
    sa.sa_handler = on_sigusr1;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = 0;
    CHECK(sigaction(SIGUSR1, &sa, NULL) == 0, "install SIGUSR1 handler");

    int epfd = epoll_create1(0);
    CHECK(epfd >= 0, "epoll_create1 ok (signal tests)");
    if (epfd < 0) {
        return;
    }

    sigset_t block_usr1;
    sigemptyset(&block_usr1);
    sigaddset(&block_usr1, SIGUSR1);

    pid_t pid = fork();
    CHECK(pid >= 0, "fork sender for masked wait");
    if (pid == 0) {
        usleep(20 * 1000);
        kill(getppid(), SIGUSR1);
        _exit(0);
    }

    struct timespec ts = {.tv_sec = 0, .tv_nsec = 100 * 1000 * 1000};
    struct epoll_event out[1];

    errno = 0;
    long r_masked = raw_epoll_pwait2(epfd, out, 1, &ts, &block_usr1, KERNEL_SIGSET_SIZE);
    CHECK(r_masked == 0, "masked SIGUSR1 does not interrupt epoll_pwait2 timeout");
    waitpid(pid, NULL, 0);

    pid = fork();
    CHECK(pid >= 0, "fork sender for unmasked wait");
    if (pid == 0) {
        usleep(20 * 1000);
        kill(getppid(), SIGUSR1);
        _exit(0);
    }

    errno = 0;
    r_masked = raw_epoll_pwait2(epfd, out, 1, NULL, NULL, KERNEL_SIGSET_SIZE);
    CHECK(r_masked == -1 && errno == EINTR, "unmasked signal interrupts with EINTR");
    waitpid(pid, NULL, 0);

    close(epfd);
}

int main(void)
{
    TEST_START("epoll_pwait2 Linux semantics");

    test_invalid_args();
    test_null_timeout_and_zero_timeout();
    test_timeout_and_sigsetsize();
    test_efault_events_ptr();
    test_timeout_monotonic_duration();
    test_maxevents_boundary();
    test_sigmask_effect_and_eintr();

    TEST_DONE();
}
