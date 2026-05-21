#include "test_framework.h"
#include <errno.h>
#include <signal.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SYS_gettid
#ifdef __NR_gettid
#define SYS_gettid __NR_gettid
#endif
#endif

#ifndef SYS_tkill
#ifdef __NR_tkill
#define SYS_tkill __NR_tkill
#endif
#endif

#ifndef SYS_gettid
#error "SYS_gettid is not available on this target"
#endif

#ifndef SYS_tkill
#error "SYS_tkill is not available on this target"
#endif

#ifndef _NSIG
#ifdef NSIG
#define _NSIG NSIG
#else
#define _NSIG 65
#endif
#endif

#define INVALID_SIGNAL_NUMBER _NSIG
#define MISSING_TID 999999

static volatile sig_atomic_t handled_count;
static volatile sig_atomic_t handled_signo;
static volatile sig_atomic_t handled_si_signo;
static volatile sig_atomic_t handled_si_pid;
static volatile sig_atomic_t handled_si_code;

static void siginfo_handler(int signo, siginfo_t *info, void *ucontext)
{
    (void)ucontext;

    handled_count++;
    handled_signo = signo;
    if (info != NULL) {
        handled_si_signo = info->si_signo;
        handled_si_pid = info->si_pid;
        handled_si_code = info->si_code;
    }
}

static long raw_gettid(void)
{
    errno = 0;
    return syscall(SYS_gettid);
}

static long raw_tkill(pid_t tid, int signo)
{
    errno = 0;
    return syscall(SYS_tkill, tid, signo);
}

static void reset_handler_state(void)
{
    handled_count = 0;
    handled_signo = 0;
    handled_si_signo = 0;
    handled_si_pid = 0;
    handled_si_code = 0;
}

static void wait_for_handler_count(sig_atomic_t count)
{
    for (int i = 0; i < 100 && handled_count < count; i++) {
        usleep(1000);
    }
}

static void install_siginfo_handler(int signo)
{
    struct sigaction sa = {0};

    sa.sa_sigaction = siginfo_handler;
    sa.sa_flags = SA_SIGINFO;
    sigemptyset(&sa.sa_mask);
    CHECK_RET(sigaction(signo, &sa, NULL), 0, "install SA_SIGINFO handler");
}

static void test_tkill_self_delivery(void)
{
    long tid = raw_gettid();
    CHECK(tid > 0, "gettid returns a positive tid");
    if (tid <= 0) {
        return;
    }

    install_siginfo_handler(SIGUSR1);

    reset_handler_state();
    CHECK_RET(raw_tkill((pid_t)tid, 0), 0, "tkill signal 0 probes current tid");
    usleep(10000);
    CHECK(handled_count == 0, "tkill signal 0 does not deliver a signal");

    CHECK_RET(raw_tkill((pid_t)tid, SIGUSR1), 0,
              "tkill sends SIGUSR1 to current tid");
    wait_for_handler_count(1);
    CHECK(handled_count == 1, "tkill invokes the installed handler once");
    CHECK(handled_signo == SIGUSR1, "tkill handler receives SIGUSR1");
    CHECK(handled_si_signo == SIGUSR1, "tkill siginfo.si_signo is SIGUSR1");
    CHECK(handled_si_pid == getpid(), "tkill siginfo.si_pid is sender pid");
#ifdef SI_TKILL
    CHECK(handled_si_code == SI_TKILL, "tkill siginfo.si_code is SI_TKILL");
#endif
}

static void test_tkill_errors(void)
{
    long tid = raw_gettid();
    CHECK(tid > 0, "gettid returns a positive tid for error tests");
    if (tid <= 0) {
        return;
    }

    CHECK(raw_tkill((pid_t)tid, INVALID_SIGNAL_NUMBER) == -1 && errno == EINVAL,
          "tkill invalid signal returns EINVAL");
    CHECK(raw_tkill(MISSING_TID, SIGUSR1) == -1 && errno == ESRCH,
          "tkill missing tid returns ESRCH");
    CHECK(raw_tkill(MISSING_TID, 0) == -1 && errno == ESRCH,
          "tkill missing tid with signal 0 returns ESRCH");
}

int main(void)
{
    TEST_START("tkill syscall semantics");

    test_tkill_self_delivery();
    test_tkill_errors();

    TEST_DONE();
}
