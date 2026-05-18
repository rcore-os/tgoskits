#include "test_framework.h"
#include <errno.h>
#include <signal.h>
#include <stddef.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SYS_rt_sigpending
#ifdef __NR_rt_sigpending
#define SYS_rt_sigpending __NR_rt_sigpending
#endif
#endif

#ifndef SYS_rt_sigpending
#error "SYS_rt_sigpending is not available on this target"
#endif

#ifndef _NSIG
#ifdef NSIG
#define _NSIG NSIG
#else
#define _NSIG 65
#endif
#endif

#define LTP_SIGSET_SIZE (_NSIG / 8)

static volatile sig_atomic_t handler_count;

static void test_handler(int signo)
{
    (void)signo;
    handler_count++;
}

static long raw_rt_sigpending(sigset_t *set, size_t sigsetsize)
{
    errno = 0;
    return syscall(SYS_rt_sigpending, set, sigsetsize);
}

static void wait_for_handler_count(sig_atomic_t count)
{
    for (int i = 0; i < 100 && handler_count < count; i++) {
        usleep(1000);
    }
}

static void check_pending_member(const sigset_t *pending, int signo, int expected,
                                 const char *msg)
{
    int member = sigismember(pending, signo);
    CHECK(member == expected, msg);
}

static void check_pending_exact_usr(const sigset_t *pending, int expect_usr1,
                                    int expect_usr2, const char *msg)
{
    int ok = 1;
    int max_signal = (int)(sizeof(sigset_t) * 8);
    if (max_signal > _NSIG) {
        max_signal = _NSIG;
    }

    for (int signo = 1; signo < max_signal; signo++) {
        int member = sigismember(pending, signo);
        if (member == -1) {
            continue;
        }

        int expected = (signo == SIGUSR1 && expect_usr1) ||
                       (signo == SIGUSR2 && expect_usr2);
        if ((member != 0) != expected) {
            ok = 0;
            break;
        }
    }

    CHECK(ok, msg);
}

static void install_usr_handlers(struct sigaction *old_usr1, struct sigaction *old_usr2)
{
    struct sigaction sa = {0};

    sa.sa_handler = test_handler;
    sigemptyset(&sa.sa_mask);
    CHECK_RET(sigaction(SIGUSR1, &sa, old_usr1), 0, "install SIGUSR1 handler");
    CHECK_RET(sigaction(SIGUSR2, &sa, old_usr2), 0, "install SIGUSR2 handler");
}

static void test_pending_masked_usr_signals(void)
{
    sigset_t mask;
    sigset_t oldmask;
    sigset_t pending;
    struct sigaction old_usr1;
    struct sigaction old_usr2;

    handler_count = 0;

    sigemptyset(&mask);
    sigaddset(&mask, SIGUSR1);
    sigaddset(&mask, SIGUSR2);

    CHECK_RET(sigprocmask(SIG_SETMASK, &mask, &oldmask), 0,
              "block SIGUSR1 and SIGUSR2");
    install_usr_handlers(&old_usr1, &old_usr2);

    sigemptyset(&pending);
    CHECK_RET(raw_rt_sigpending(&pending, LTP_SIGSET_SIZE), 0,
              "rt_sigpending initially succeeds");
    check_pending_exact_usr(&pending, 0, 0, "initially no signal is pending");

    CHECK_RET(kill(getpid(), SIGUSR1), 0, "send blocked SIGUSR1");
    usleep(10000);
    CHECK(handler_count == 0, "SIGUSR1 handler is deferred while blocked");

    sigemptyset(&pending);
    CHECK_RET(raw_rt_sigpending(&pending, LTP_SIGSET_SIZE), 0,
              "rt_sigpending after SIGUSR1 succeeds");
    check_pending_member(&pending, SIGUSR1, 1, "SIGUSR1 is pending");
    check_pending_member(&pending, SIGUSR2, 0, "SIGUSR2 is not pending yet");
    check_pending_exact_usr(&pending, 1, 0, "only SIGUSR1 is pending");

    CHECK_RET(kill(getpid(), SIGUSR2), 0, "send blocked SIGUSR2");
    usleep(10000);
    CHECK(handler_count == 0, "SIGUSR2 handler is also deferred while blocked");

    sigemptyset(&pending);
    CHECK_RET(raw_rt_sigpending(&pending, LTP_SIGSET_SIZE), 0,
              "rt_sigpending after SIGUSR1 and SIGUSR2 succeeds");
    check_pending_member(&pending, SIGUSR1, 1, "SIGUSR1 remains pending");
    check_pending_member(&pending, SIGUSR2, 1, "SIGUSR2 is pending");
    check_pending_exact_usr(&pending, 1, 1, "only SIGUSR1 and SIGUSR2 are pending");

    CHECK_RET(sigprocmask(SIG_SETMASK, &oldmask, NULL), 0,
              "restore original signal mask");
    wait_for_handler_count(2);
    CHECK(handler_count == 2,
          "unblocking delivers one handler call for each pending signal");

    CHECK_RET(sigaction(SIGUSR1, &old_usr1, NULL), 0, "restore SIGUSR1 handler");
    CHECK_RET(sigaction(SIGUSR2, &old_usr2, NULL), 0, "restore SIGUSR2 handler");
}

static void test_invalid_pending_pointer(void)
{
    CHECK(raw_rt_sigpending((sigset_t *)-1, LTP_SIGSET_SIZE) == -1 &&
              errno == EFAULT,
          "rt_sigpending invalid sigset pointer returns EFAULT");
}

int main(void)
{
    TEST_START("rt_sigpending syscall semantics");

    test_pending_masked_usr_signals();
    test_invalid_pending_pointer();

    TEST_DONE();
}
