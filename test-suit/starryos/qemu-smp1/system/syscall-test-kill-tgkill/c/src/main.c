#include "test_framework.h"
#include <errno.h>
#include <signal.h>
#include <stddef.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SYS_gettid
#ifdef __NR_gettid
#define SYS_gettid __NR_gettid
#endif
#endif

#ifndef SYS_kill
#ifdef __NR_kill
#define SYS_kill __NR_kill
#endif
#endif

#ifndef SYS_tgkill
#ifdef __NR_tgkill
#define SYS_tgkill __NR_tgkill
#endif
#endif

#ifndef SYS_gettid
#error "SYS_gettid is not available on this target"
#endif

#ifndef SYS_kill
#error "SYS_kill is not available on this target"
#endif

#ifndef SYS_tgkill
#error "SYS_tgkill is not available on this target"
#endif

#ifndef _NSIG
#ifdef NSIG
#define _NSIG NSIG
#else
#define _NSIG 65
#endif
#endif

#define INVALID_SIGNAL_NUMBER _NSIG
#define MISSING_PID 999999
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

static long raw_kill(pid_t pid, int signo)
{
    errno = 0;
    return syscall(SYS_kill, pid, signo);
}

static long raw_tgkill(pid_t tgid, pid_t tid, int signo)
{
    errno = 0;
    return syscall(SYS_tgkill, tgid, tid, signo);
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

static void test_kill_self_delivery(void)
{
    install_siginfo_handler(SIGUSR1);

    reset_handler_state();
    CHECK_RET(raw_kill(getpid(), 0), 0, "kill signal 0 probes current process");
    usleep(10000);
    CHECK(handled_count == 0, "kill signal 0 does not deliver a signal");

    CHECK_RET(raw_kill(getpid(), SIGUSR1), 0, "kill sends SIGUSR1 to self");
    wait_for_handler_count(1);
    CHECK(handled_count == 1, "kill invokes the installed handler once");
    CHECK(handled_signo == SIGUSR1, "kill handler receives SIGUSR1");
    CHECK(handled_si_signo == SIGUSR1, "kill siginfo.si_signo is SIGUSR1");
    CHECK(handled_si_pid == getpid(), "kill siginfo.si_pid is sender pid");
#ifdef SI_USER
    CHECK(handled_si_code == SI_USER, "kill siginfo.si_code is SI_USER");
#endif
}

static void test_kill_errors(void)
{
    CHECK(raw_kill(MISSING_PID, 0) == -1 && errno == ESRCH,
          "kill missing pid with signal 0 returns ESRCH");
    CHECK(raw_kill(getpid(), INVALID_SIGNAL_NUMBER) == -1 && errno == EINVAL,
          "kill invalid signal returns EINVAL");
}

static void test_tgkill_self_delivery(void)
{
    long tid = raw_gettid();
    CHECK(tid > 0, "gettid returns a positive tid");
    if (tid <= 0) {
        return;
    }

    install_siginfo_handler(SIGUSR2);

    reset_handler_state();
    CHECK_RET(raw_tgkill(getpid(), (pid_t)tid, 0), 0,
              "tgkill signal 0 probes current thread");
    usleep(10000);
    CHECK(handled_count == 0, "tgkill signal 0 does not deliver a signal");

    CHECK_RET(raw_tgkill(getpid(), (pid_t)tid, SIGUSR2), 0,
              "tgkill sends SIGUSR2 to current tid");
    wait_for_handler_count(1);
    CHECK(handled_count == 1, "tgkill invokes the installed handler once");
    CHECK(handled_signo == SIGUSR2, "tgkill handler receives SIGUSR2");
    CHECK(handled_si_signo == SIGUSR2, "tgkill siginfo.si_signo is SIGUSR2");
    CHECK(handled_si_pid == getpid(), "tgkill siginfo.si_pid is sender pid");
#ifdef SI_TKILL
    CHECK(handled_si_code == SI_TKILL, "tgkill siginfo.si_code is SI_TKILL");
#endif
}

static void test_tgkill_errors(void)
{
    long tid = raw_gettid();
    CHECK(tid > 0, "gettid returns a positive tid for error tests");
    if (tid <= 0) {
        return;
    }

    CHECK(raw_tgkill(getpid(), (pid_t)tid, INVALID_SIGNAL_NUMBER) == -1 &&
              errno == EINVAL,
          "tgkill invalid signal returns EINVAL");
    CHECK(raw_tgkill(getpid(), MISSING_TID, SIGUSR2) == -1 && errno == ESRCH,
          "tgkill missing tid returns ESRCH");
    CHECK(raw_tgkill(MISSING_PID, (pid_t)tid, 0) == -1 && errno == ESRCH,
          "tgkill missing tgid returns ESRCH");
}

int main(void)
{
    TEST_START("kill/tgkill syscall semantics");

    test_kill_self_delivery();
    test_kill_errors();
    test_tgkill_self_delivery();
    test_tgkill_errors();

    TEST_DONE();
}
