#include "test_framework.h"
#include <errno.h>
#include <signal.h>
#include <stddef.h>
#include <string.h>
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

static long raw_rt_sigprocmask(int how, const sigset_t *set, sigset_t *oldset,
                               size_t sigsetsize)
{
    errno = 0;
    return syscall(SYS_rt_sigprocmask, how, set, oldset, sigsetsize);
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

/*
 * The raw syscall exchanges only the 8-byte kernel sigset_t, while the libc
 * sigset_t can be larger. Initialize every byte, but only compare the ABI
 * range the syscall writes.
 */
static void test_sigprocmask_semantics(void)
{
    sigset_t original;
    sigset_t current;
    sigset_t set;
    sigset_t output;
    sigset_t expected_output;
    const int invalid_how = 0x40000000;
    const size_t sigset_bytes = LTP_SIGSET_SIZE;

    memset(&original, 0xa5, sizeof(original));
    memset(&current, 0xa5, sizeof(current));
    if (raw_rt_sigprocmask(SIG_SETMASK, NULL, &original,
                           sigset_bytes) != 0) {
        CHECK(0, "capture current mask before rt_sigprocmask semantics");
        return;
    }
    CHECK(1, "capture current mask before rt_sigprocmask semantics");

    sigemptyset(&set);
    CHECK_RET(sigaddset(&set, SIGUSR1), 0,
              "prepare valid input for invalid rt_sigprocmask");

    memset(&output, 0xa5, sizeof(output));
    for (size_t i = 0; i < sigset_bytes; i++) {
        ((unsigned char *)&output)[i] =
            (unsigned char)~((const unsigned char *)&original)[i];
    }
    expected_output = output;

    CHECK(raw_rt_sigprocmask(invalid_how, &set, &output,
                             sigset_bytes) == -1 &&
              errno == EINVAL,
          "rt_sigprocmask rejects invalid how with EINVAL");
    CHECK(memcmp(&output, &expected_output, sigset_bytes) == 0,
          "invalid rt_sigprocmask does not write oldset");

    errno = 0;
    CHECK(syscall(SYS_rt_sigprocmask, invalid_how, &set, (sigset_t *)-1,
                  sigset_bytes) == -1 &&
              errno == EINVAL,
          "invalid how is reported before an invalid oldset pointer");

    memset(&output, 0xa5, sizeof(output));
    errno = 0;
    CHECK(raw_rt_sigprocmask(invalid_how, (sigset_t *)-1, &output,
                             sigset_bytes) == -1 &&
              errno == EFAULT,
          "rt_sigprocmask copies the input set before validating how");
    CHECK(((unsigned char *)&output)[0] == 0xa5,
          "input EFAULT does not write the old mask");

    memset(&output, 0xa5, sizeof(output));
    errno = 0;
    CHECK(raw_rt_sigprocmask(invalid_how, NULL, &output,
                             sigset_bytes) == 0,
          "rt_sigprocmask ignores how when set is NULL");
    CHECK(memcmp(&output, &original, sigset_bytes) == 0,
          "set=NULL still returns the current mask");

    sigemptyset(&set);
    CHECK_RET(sigaddset(&set, SIGUSR1), 0,
              "add SIGUSR1 for committed-mask EFAULT test");
    CHECK_RET(sigaddset(&set, SIGUSR2), 0,
              "add SIGUSR2 for committed-mask EFAULT test");

    errno = 0;
    CHECK(raw_rt_sigprocmask(SIG_SETMASK, &set, (sigset_t *)-1,
                             sigset_bytes) == -1 &&
              errno == EFAULT,
          "rt_sigprocmask reports oldset EFAULT after committing the new mask");

    CHECK(raw_rt_sigprocmask(SIG_SETMASK, NULL, &current,
                             sigset_bytes) == 0 &&
              memcmp(&current, &set, sigset_bytes) == 0,
          "oldset EFAULT keeps the successfully committed signal mask");

    CHECK(raw_rt_sigprocmask(SIG_SETMASK, &original, NULL,
                             sigset_bytes) == 0,
          "restore original signal mask after rt_sigprocmask semantics");
}

static void test_short_sigset_sizes(void)
{
    unsigned char pending[LTP_SIGSET_SIZE];
    sigset_t expected;
    sigset_t mask;
    sigset_t oldmask;
    struct sigaction old_usr1;
    struct sigaction old_usr2;

    CHECK_RET(raw_rt_sigpending(NULL, 0), 0,
              "rt_sigpending accepts a null pointer for a zero-length set");

    handler_count = 0;
    sigemptyset(&mask);
    CHECK_RET(sigaddset(&mask, SIGUSR1), 0, "add SIGUSR1 to short-size mask");
    CHECK_RET(sigaddset(&mask, SIGUSR2), 0, "add SIGUSR2 to short-size mask");
    CHECK_RET(sigprocmask(SIG_SETMASK, &mask, &oldmask), 0,
              "block SIGUSR1 and SIGUSR2 for short-size test");
    install_usr_handlers(&old_usr1, &old_usr2);
    CHECK_RET(kill(getpid(), SIGUSR1), 0, "send blocked SIGUSR1 for short-size test");
    CHECK_RET(kill(getpid(), SIGUSR2), 0, "send blocked SIGUSR2 for short-size test");
    usleep(10000);
    CHECK(handler_count == 0, "short-size test signals remain pending while blocked");

    sigemptyset(&expected);
    CHECK_RET(sigaddset(&expected, SIGUSR1), 0, "record pending SIGUSR1 bit");
    CHECK_RET(sigaddset(&expected, SIGUSR2), 0, "record pending SIGUSR2 bit");
    memset(pending, 0xa5, sizeof(pending));
    CHECK_RET(raw_rt_sigpending((sigset_t *)pending, LTP_SIGSET_SIZE - 1), 0,
              "rt_sigpending accepts a short signal set");
    CHECK(memcmp(pending, &expected, LTP_SIGSET_SIZE - 1) == 0,
          "rt_sigpending writes the pending-mask prefix for a short signal set");
    CHECK(pending[LTP_SIGSET_SIZE - 1] == 0xa5,
          "rt_sigpending writes only the requested signal-set prefix");

    CHECK_RET(sigprocmask(SIG_SETMASK, &oldmask, NULL), 0,
              "restore original mask after short-size test");
    wait_for_handler_count(2);
    CHECK(handler_count == 2, "short-size test unblocks both pending signals");
    CHECK_RET(sigaction(SIGUSR1, &old_usr1, NULL), 0,
              "restore SIGUSR1 handler after short-size test");
    CHECK_RET(sigaction(SIGUSR2, &old_usr2, NULL), 0,
              "restore SIGUSR2 handler after short-size test");

    CHECK(raw_rt_sigpending((sigset_t *)pending, LTP_SIGSET_SIZE + 1) == -1 &&
              errno == EINVAL,
          "rt_sigpending rejects a signal set larger than the kernel size");
}

int main(void)
{
    TEST_START("rt_sigpending syscall semantics");

    test_pending_masked_usr_signals();
    test_invalid_pending_pointer();
    test_sigprocmask_semantics();
    test_short_sigset_sizes();

    TEST_DONE();
}
