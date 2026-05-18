#include "test_framework.h"
#include <errno.h>
#include <signal.h>
#include <stddef.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SYS_rt_sigprocmask
#ifdef __NR_rt_sigprocmask
#define SYS_rt_sigprocmask __NR_rt_sigprocmask
#endif
#endif

#ifndef SYS_rt_sigprocmask
#error "SYS_rt_sigprocmask is not available on this target"
#endif

#ifndef _NSIG
#ifdef NSIG
#define _NSIG NSIG
#else
#define _NSIG 65
#endif
#endif

#define KERNEL_SIGSET_WORDS 1
#define KERNEL_SIGSET_SIZE (sizeof(unsigned long) * KERNEL_SIGSET_WORDS)
#define LTP_SIGSET_SIZE (_NSIG / 8)

static volatile sig_atomic_t handled_count;
static volatile sig_atomic_t handled_signo;

static void test_handler(int signo)
{
    handled_signo = signo;
    handled_count++;
}

static long raw_rt_sigprocmask(int how, const sigset_t *set, sigset_t *oldset,
                               size_t sigsetsize)
{
    errno = 0;
    return syscall(SYS_rt_sigprocmask, how, set, oldset, sigsetsize);
}

static void reset_handler_state(void)
{
    handled_count = 0;
    handled_signo = 0;
}

static void wait_for_handler_count(sig_atomic_t count)
{
    for (int i = 0; i < 100 && handled_count < count; i++) {
        usleep(1000);
    }
}

static void install_handler(int signo)
{
    struct sigaction sa = {0};
    sa.sa_handler = test_handler;
    sigemptyset(&sa.sa_mask);
    CHECK_RET(sigaction(signo, &sa, NULL), 0, "install signal handler");
}

static void clear_signal_mask(void)
{
    sigset_t empty;
    sigemptyset(&empty);
    CHECK_RET(raw_rt_sigprocmask(SIG_SETMASK, &empty, NULL, LTP_SIGSET_SIZE), 0,
              "clear signal mask");
}

static int current_mask_has(int signo)
{
    sigset_t current;
    sigemptyset(&current);
    long ret = raw_rt_sigprocmask(SIG_SETMASK, NULL, &current, LTP_SIGSET_SIZE);
    CHECK(ret == 0, "query current signal mask");
    if (ret != 0) {
        return -1;
    }
    return sigismember(&current, signo);
}

static void test_block_defer_and_unblock(void)
{
    sigset_t mask;
    sigset_t oldmask;
    sigset_t pending;

    install_handler(SIGUSR1);
    clear_signal_mask();

    sigemptyset(&mask);
    sigaddset(&mask, SIGUSR1);
    sigemptyset(&oldmask);

    CHECK_RET(raw_rt_sigprocmask(SIG_BLOCK, &mask, &oldmask, LTP_SIGSET_SIZE), 0,
              "SIG_BLOCK adds SIGUSR1 to the mask");
    CHECK(sigismember(&oldmask, SIGUSR1) == 0,
          "SIG_BLOCK oldset reports SIGUSR1 was unblocked");
    CHECK(current_mask_has(SIGUSR1) == 1, "SIGUSR1 is blocked after SIG_BLOCK");

    reset_handler_state();
    CHECK_RET(kill(getpid(), SIGUSR1), 0, "send SIGUSR1 while blocked");
    usleep(20000);
    CHECK(handled_count == 0, "blocked SIGUSR1 is not delivered immediately");

    sigemptyset(&pending);
    CHECK_RET(sigpending(&pending), 0, "query pending signal set");
    CHECK(sigismember(&pending, SIGUSR1) == 1, "blocked SIGUSR1 becomes pending");

    sigemptyset(&oldmask);
    CHECK_RET(raw_rt_sigprocmask(SIG_UNBLOCK, &mask, &oldmask, LTP_SIGSET_SIZE), 0,
              "SIG_UNBLOCK removes SIGUSR1 from the mask");
    CHECK(sigismember(&oldmask, SIGUSR1) == 1,
          "SIG_UNBLOCK oldset reports SIGUSR1 was blocked");
    wait_for_handler_count(1);
    CHECK(handled_count == 1 && handled_signo == SIGUSR1,
          "pending SIGUSR1 is delivered after unblock");

    clear_signal_mask();
}

static void test_setmask_replaces_mask(void)
{
    sigset_t usr1;
    sigset_t usr2;
    sigset_t oldmask;

    clear_signal_mask();

    sigemptyset(&usr1);
    sigaddset(&usr1, SIGUSR1);
    CHECK_RET(raw_rt_sigprocmask(SIG_BLOCK, &usr1, NULL, LTP_SIGSET_SIZE), 0,
              "block SIGUSR1 before SIG_SETMASK");

    sigemptyset(&usr2);
    sigaddset(&usr2, SIGUSR2);
    sigemptyset(&oldmask);
    CHECK_RET(raw_rt_sigprocmask(SIG_SETMASK, &usr2, &oldmask, LTP_SIGSET_SIZE), 0,
              "SIG_SETMASK replaces the current mask");
    CHECK(sigismember(&oldmask, SIGUSR1) == 1,
          "SIG_SETMASK oldset contains the previous SIGUSR1 mask");
    CHECK(sigismember(&oldmask, SIGUSR2) == 0,
          "SIG_SETMASK oldset does not contain the new SIGUSR2 mask");
    CHECK(current_mask_has(SIGUSR1) == 0, "SIG_SETMASK removed SIGUSR1");
    CHECK(current_mask_has(SIGUSR2) == 1, "SIG_SETMASK installed SIGUSR2");

    clear_signal_mask();
}

static void test_null_set_queries_oldmask(void)
{
    sigset_t mask;
    sigset_t oldmask;

    clear_signal_mask();

    sigemptyset(&mask);
    sigaddset(&mask, SIGUSR2);
    CHECK_RET(raw_rt_sigprocmask(SIG_BLOCK, &mask, NULL, LTP_SIGSET_SIZE), 0,
              "block SIGUSR2 before NULL set query");

    sigemptyset(&oldmask);
    CHECK_RET(raw_rt_sigprocmask(-1, NULL, &oldmask, LTP_SIGSET_SIZE), 0,
              "NULL set query ignores how");
    CHECK(sigismember(&oldmask, SIGUSR2) == 1,
          "NULL set query returns current blocked signals");
    CHECK(current_mask_has(SIGUSR2) == 1,
          "NULL set query leaves the current mask unchanged");

    clear_signal_mask();
}

static void test_invalid_inputs(void)
{
    sigset_t mask;

    sigemptyset(&mask);
    sigaddset(&mask, SIGUSR1);

    CHECK(raw_rt_sigprocmask(0x7f, &mask, NULL, LTP_SIGSET_SIZE) == -1 &&
              errno == EINVAL,
          "invalid how with non-NULL set returns EINVAL");
    CHECK(raw_rt_sigprocmask(SIG_BLOCK, (const sigset_t *)-1, NULL,
                             LTP_SIGSET_SIZE) == -1 &&
              errno == EFAULT,
          "invalid set pointer returns EFAULT");
    CHECK(raw_rt_sigprocmask(SIG_SETMASK, NULL, (sigset_t *)-1,
                             LTP_SIGSET_SIZE) == -1 &&
              errno == EFAULT,
          "invalid oldset pointer returns EFAULT");
    CHECK(raw_rt_sigprocmask(SIG_BLOCK, &mask, NULL, KERNEL_SIGSET_SIZE - 1) == -1 &&
              errno == EINVAL,
          "too-small sigsetsize returns EINVAL");
}

int main(void)
{
    TEST_START("rt_sigprocmask syscall semantics");

    clear_signal_mask();
    test_block_defer_and_unblock();
    test_setmask_replaces_mask();
    test_null_set_queries_oldmask();
    test_invalid_inputs();
    clear_signal_mask();

    TEST_DONE();
}
