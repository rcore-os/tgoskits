#define _GNU_SOURCE

#include <errno.h>
#include <signal.h>
#include <pthread.h>
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <stdatomic.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/signalfd.h>
#include <time.h>
#include <unistd.h>

static int passed;
static int failed;
static _Atomic int waiter_entered;

struct waiter_context {
    int epoll_fd;
    int signal_fd;
    int wait_result;
    int wait_errno;
    ssize_t read_length;
    int read_errno;
    struct epoll_event event;
    struct signalfd_siginfo signal_info;
};

static void expect_true(int condition, const char *name)
{
    if (condition) {
        printf("PASS: %s\n", name);
        passed++;
        return;
    }
    printf("FAIL: %s: errno=%d (%s)\n", name, errno, strerror(errno));
    failed++;
}

static void *wait_for_signalfd_event(void *opaque)
{
    struct waiter_context *context = opaque;

    atomic_store_explicit(&waiter_entered, 1, memory_order_release);
    errno = 0;
    context->wait_result = epoll_wait(context->epoll_fd, &context->event, 1, 2000);
    context->wait_errno = errno;
    if (context->wait_result != 1) {
        return NULL;
    }

    errno = 0;
    context->read_length = read(
        context->signal_fd,
        &context->signal_info,
        sizeof(context->signal_info)
    );
    context->read_errno = errno;
    return NULL;
}

static int wait_for_epoll_waiter(void)
{
    const struct timespec settle = {
        .tv_sec = 0,
        .tv_nsec = 100 * 1000 * 1000,
    };

    while (atomic_load_explicit(&waiter_entered, memory_order_acquire) == 0) {
        sched_yield();
    }

    return nanosleep(&settle, NULL);
}

int main(void)
{
    printf("=== bugfix-signalfd-epoll-wakeup ===\n");

    sigset_t mask;
    sigemptyset(&mask);
    sigaddset(&mask, SIGUSR1);
    expect_true(sigprocmask(SIG_BLOCK, &mask, NULL) == 0, "block SIGUSR1");

    int signal_fd = signalfd(-1, &mask, SFD_CLOEXEC | SFD_NONBLOCK);
    expect_true(signal_fd >= 0, "create nonblocking signalfd");

    int epoll_fd = epoll_create1(EPOLL_CLOEXEC);
    expect_true(epoll_fd >= 0, "create epoll");

    struct epoll_event interest = {
        .events = EPOLLIN,
        .data.fd = signal_fd,
    };
    expect_true(signal_fd >= 0 && epoll_fd >= 0 &&
                    epoll_ctl(epoll_fd, EPOLL_CTL_ADD, signal_fd,
                              &interest) == 0,
                "register signalfd with epoll");

    struct waiter_context context = {
        .epoll_fd = epoll_fd,
        .signal_fd = signal_fd,
        .wait_result = -1,
        .wait_errno = 0,
        .read_length = -1,
        .read_errno = 0,
    };
    pthread_t waiter;
    int waiter_started = signal_fd >= 0 && epoll_fd >= 0 &&
                         pthread_create(&waiter, NULL, wait_for_signalfd_event, &context) == 0;
    expect_true(waiter_started, "start epoll_wait thread");

    if (waiter_started) {
        expect_true(wait_for_epoll_waiter() == 0, "wait for epoll_wait thread to block");
        expect_true(pthread_kill(waiter, SIGUSR1) == 0,
                    "send blocked SIGUSR1 to epoll_wait thread");
        expect_true(pthread_join(waiter, NULL) == 0, "join epoll_wait thread");
        expect_true(context.wait_result == 1 && context.event.data.fd == signal_fd &&
                        (context.event.events & EPOLLIN) != 0,
                    "epoll wakes when target thread receives blocked SIGUSR1");
        expect_true(context.read_length == (ssize_t)sizeof(context.signal_info),
                    "read SIGUSR1 from signalfd in epoll_wait thread");
        expect_true(context.read_length == (ssize_t)sizeof(context.signal_info) &&
                        context.signal_info.ssi_signo == SIGUSR1,
                    "signalfd reports target thread SIGUSR1");
    }

    if (epoll_fd >= 0) {
        close(epoll_fd);
    }
    if (signal_fd >= 0) {
        close(signal_fd);
    }

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("STARRY_SIGNALFD_EPOLL_WAKEUP_PASSED\n");
        printf("STARRY_GROUPED_TEST_PASSED: bugfix-signalfd-epoll-wakeup\n");
        return EXIT_SUCCESS;
    }
    printf("STARRY_GROUPED_TEST_FAILED: bugfix-signalfd-epoll-wakeup\n");
    return EXIT_FAILURE;
}
