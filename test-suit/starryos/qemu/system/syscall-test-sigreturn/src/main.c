#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int passed;
static int failed;
static volatile sig_atomic_t handler_called;
static volatile sig_atomic_t handler_saw_usr1_blocked;
static volatile sig_atomic_t handler_saw_usr2_blocked;

#define CHECK(label, cond, fmt, ...)                                      \
    do {                                                                  \
        if (cond) {                                                       \
            printf("  PASS | rt_sigreturn | %s\n", label);               \
            passed++;                                                     \
        } else {                                                          \
            printf("  FAIL | rt_sigreturn | %s | " fmt "\n", label,      \
                   ##__VA_ARGS__);                                        \
            failed++;                                                     \
        }                                                                 \
    } while (0)

static int mask_has(int signo)
{
    sigset_t current;
    if (sigprocmask(SIG_SETMASK, NULL, &current) != 0) {
        return -1;
    }
    return sigismember(&current, signo);
}

static void handler(int signo)
{
    sigset_t current;
    handler_called = (signo == SIGUSR1);
    if (sigprocmask(SIG_SETMASK, NULL, &current) == 0) {
        handler_saw_usr1_blocked = sigismember(&current, SIGUSR1) == 1;
        handler_saw_usr2_blocked = sigismember(&current, SIGUSR2) == 1;
    }
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("=== test-sigreturn: handler return restores user mask ===\n");

    sigset_t unblock_signals;
    sigemptyset(&unblock_signals);
    sigaddset(&unblock_signals, SIGUSR1);
    sigaddset(&unblock_signals, SIGUSR2);
    sigprocmask(SIG_UNBLOCK, &unblock_signals, NULL);
    CHECK("SIGUSR1 starts unblocked", mask_has(SIGUSR1) == 0,
          "mask_has=%d", mask_has(SIGUSR1));
    CHECK("SIGUSR2 starts unblocked", mask_has(SIGUSR2) == 0,
          "mask_has=%d", mask_has(SIGUSR2));

    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = handler;
    sigemptyset(&sa.sa_mask);
    sigaddset(&sa.sa_mask, SIGUSR2);
    CHECK("install handler with temporary SIGUSR2 mask",
          sigaction(SIGUSR1, &sa, NULL) == 0,
          "errno=%d (%s)", errno, strerror(errno));

    CHECK("raise SIGUSR1", raise(SIGUSR1) == 0, "errno=%d (%s)", errno,
          strerror(errno));
    CHECK("handler ran and returned to user flow", handler_called,
          "handler_called=%d", handler_called);
    CHECK("handler observed current signal temporarily blocked",
          handler_saw_usr1_blocked,
          "handler_saw_usr1_blocked=%d", handler_saw_usr1_blocked);
    CHECK("handler observed action mask", handler_saw_usr2_blocked,
          "handler_saw_usr2_blocked=%d", handler_saw_usr2_blocked);
    CHECK("rt_sigreturn restored current signal mask", mask_has(SIGUSR1) == 0,
          "mask_has=%d", mask_has(SIGUSR1));
    CHECK("rt_sigreturn restored previous mask", mask_has(SIGUSR2) == 0,
          "mask_has=%d", mask_has(SIGUSR2));

    printf("=== result: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("TEST PASSED: test-sigreturn\n");
    } else {
        printf("TEST FAILED: test-sigreturn\n");
    }
    return failed == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}
