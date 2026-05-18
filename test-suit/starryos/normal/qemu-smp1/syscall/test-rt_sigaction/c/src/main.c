#include "test_framework.h"
#include <errno.h>
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SYS_rt_sigaction
#ifdef __NR_rt_sigaction
#define SYS_rt_sigaction __NR_rt_sigaction
#endif
#endif

#ifndef SYS_rt_sigaction
#error "SYS_rt_sigaction is not available on this target"
#endif

#ifndef SA_NOMASK
#define SA_NOMASK SA_NODEFER
#endif

#ifndef SA_RESTORER
#define SA_RESTORER 0x04000000
#endif

#define KERNEL_SIGSET_WORDS 1
#define KERNEL_SIGSET_SIZE (sizeof(unsigned long) * KERNEL_SIGSET_WORDS)
#define LTP_SIGSET_SIZE (_NSIG / 8)

struct kernel_sigaction {
    void (*handler)(int);
    unsigned long flags;
#if defined(__x86_64__) || defined(__aarch64__)
    void (*restorer)(void);
#endif
    unsigned long mask[KERNEL_SIGSET_WORDS];
};

#if defined(__x86_64__)
__asm__(
    ".text\n"
    ".global rt_sigaction_restore_rt\n"
    ".type rt_sigaction_restore_rt, @function\n"
    "rt_sigaction_restore_rt:\n"
    "    mov $15, %rax\n"
    "    syscall\n"
    ".size rt_sigaction_restore_rt, . - rt_sigaction_restore_rt\n");

extern void rt_sigaction_restore_rt(void);
#endif

static volatile sig_atomic_t handled_signo;
static volatile sig_atomic_t handled_count;

static void rt_handler(int signo)
{
    handled_signo = signo;
    handled_count++;
}

static void reset_handler_state(void)
{
    handled_signo = 0;
    handled_count = 0;
}

static void ksigemptyset(unsigned long mask[KERNEL_SIGSET_WORDS])
{
    for (size_t i = 0; i < KERNEL_SIGSET_WORDS; i++) {
        mask[i] = 0;
    }
}

static void ksigaddset(unsigned long mask[KERNEL_SIGSET_WORDS], int signo)
{
    mask[(size_t)(signo - 1) / (8 * sizeof(unsigned long))] |=
        1UL << ((size_t)(signo - 1) % (8 * sizeof(unsigned long)));
}

static int ksigismember(const unsigned long mask[KERNEL_SIGSET_WORDS], int signo)
{
    return (mask[(size_t)(signo - 1) / (8 * sizeof(unsigned long))] &
            (1UL << ((size_t)(signo - 1) % (8 * sizeof(unsigned long))))) != 0;
}

static long raw_rt_sigaction(int signo, const struct kernel_sigaction *act,
                             struct kernel_sigaction *oldact, size_t sigsetsize)
{
    errno = 0;
    return syscall(SYS_rt_sigaction, signo, act, oldact, sigsetsize);
}

static void init_action(struct kernel_sigaction *act, int signo, unsigned long flags)
{
    *act = (struct kernel_sigaction){0};
    act->handler = rt_handler;
    act->flags = flags;
#if defined(__x86_64__)
    act->flags |= SA_RESTORER;
    act->restorer = rt_sigaction_restore_rt;
#endif
    ksigemptyset(act->mask);
    ksigaddset(act->mask, signo);
}

static void restore_default(int signo)
{
    struct kernel_sigaction act = {0};
    raw_rt_sigaction(signo, &act, NULL, LTP_SIGSET_SIZE);
}

static void test_install_realtime_handlers(void)
{
    const unsigned long flags[] = {
        SA_RESETHAND | SA_SIGINFO,
        SA_RESETHAND,
        SA_RESETHAND | SA_SIGINFO,
        SA_RESETHAND | SA_SIGINFO,
        SA_NOMASK,
    };
    const char *flag_names[] = {
        "SA_RESETHAND|SA_SIGINFO",
        "SA_RESETHAND",
        "SA_RESETHAND|SA_SIGINFO",
        "SA_RESETHAND|SA_SIGINFO",
        "SA_NOMASK",
    };

    for (int signo = SIGRTMIN; signo <= SIGRTMAX; signo++) {
        for (size_t i = 0; i < sizeof(flags) / sizeof(flags[0]); i++) {
            struct kernel_sigaction act;
            struct kernel_sigaction oldact;
            init_action(&act, signo, flags[i]);

            long ret = raw_rt_sigaction(signo, &act, &oldact, LTP_SIGSET_SIZE);
            char msg[128];
            snprintf(msg, sizeof(msg), "install %s handler for realtime signal %d",
                     flag_names[i], signo);
            CHECK(ret == 0, msg);
            if (ret != 0) {
                continue;
            }

            reset_handler_state();
            CHECK_RET(kill(getpid(), signo), 0, "deliver installed realtime signal");
            CHECK(handled_count == 1 && handled_signo == signo,
                  "realtime signal handler was called once");

            restore_default(signo);
        }
    }
}

static void test_oldact_roundtrip(void)
{
    const int signo = SIGRTMIN;
    struct kernel_sigaction act;
    struct kernel_sigaction oldact;
    struct kernel_sigaction current;

    init_action(&act, signo, SA_RESTART | SA_NODEFER);
    CHECK_RET(raw_rt_sigaction(signo, &act, &oldact, LTP_SIGSET_SIZE), 0,
              "install handler and collect old action");

    CHECK_RET(raw_rt_sigaction(signo, NULL, &current, LTP_SIGSET_SIZE), 0,
              "query current handler through oldact");
    CHECK(current.handler == rt_handler, "oldact query returns installed handler");
    CHECK((current.flags & (SA_RESTART | SA_NODEFER)) == (SA_RESTART | SA_NODEFER),
          "oldact query preserves installed flags");
    CHECK(ksigismember(current.mask, signo), "oldact query preserves installed mask");

    restore_default(signo);
}

static void test_invalid_action_pointer(void)
{
    for (int signo = SIGRTMIN; signo <= SIGRTMAX; signo++) {
        long ret = raw_rt_sigaction(signo, (const struct kernel_sigaction *)-1,
                                    NULL, LTP_SIGSET_SIZE);
        char msg[96];
        snprintf(msg, sizeof(msg), "invalid act pointer for realtime signal %d returns EFAULT",
                 signo);
        CHECK(ret == -1 && errno == EFAULT, msg);
    }
}

static void test_invalid_oldaction_pointer(void)
{
    for (int signo = SIGRTMIN; signo <= SIGRTMAX; signo++) {
        long ret = raw_rt_sigaction(signo, NULL, (struct kernel_sigaction *)-1,
                                    LTP_SIGSET_SIZE);
        char msg[96];
        snprintf(msg, sizeof(msg), "invalid oldact pointer for realtime signal %d returns EFAULT",
                 signo);
        CHECK(ret == -1 && errno == EFAULT, msg);
    }
}

static void test_invalid_sigsetsize(void)
{
    struct kernel_sigaction act;

    for (int signo = SIGRTMIN; signo <= SIGRTMAX; signo++) {
        init_action(&act, signo, SA_RESETHAND);

        /*
         * LTP's rt_sigaction03 uses -1 here. Starry deliberately accepts
         * libc-sized masks larger than the kernel mask, so the stable invalid
         * boundary is a size smaller than the kernel sigset.
         */
        long ret = raw_rt_sigaction(signo, &act, NULL, KERNEL_SIGSET_SIZE - 1);
        char msg[96];
        snprintf(msg, sizeof(msg), "invalid sigsetsize for realtime signal %d returns EINVAL",
                 signo);
        CHECK(ret == -1 && errno == EINVAL, msg);
    }
}

static void test_invalid_signals(void)
{
    struct kernel_sigaction act;
    init_action(&act, SIGUSR1, 0);

    CHECK(raw_rt_sigaction(0, &act, NULL, LTP_SIGSET_SIZE) == -1 && errno == EINVAL,
          "signal 0 is rejected");
    CHECK(raw_rt_sigaction(SIGKILL, &act, NULL, LTP_SIGSET_SIZE) == -1 && errno == EINVAL,
          "SIGKILL handler install is rejected");
    CHECK(raw_rt_sigaction(SIGSTOP, &act, NULL, LTP_SIGSET_SIZE) == -1 && errno == EINVAL,
          "SIGSTOP handler install is rejected");
}

int main(void)
{
    TEST_START("rt_sigaction syscall semantics");

    test_install_realtime_handlers();
    test_oldact_roundtrip();
    test_invalid_action_pointer();
    test_invalid_oldaction_pointer();
    test_invalid_sigsetsize();
    test_invalid_signals();

    TEST_DONE();
}
