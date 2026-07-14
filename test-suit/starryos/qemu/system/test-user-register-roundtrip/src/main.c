#define _GNU_SOURCE

#include <elf.h>
#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ptrace.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef NT_PRSTATUS
#define NT_PRSTATUS 1
#endif
#ifndef PTRACE_GETREGSET
#define PTRACE_GETREGSET 0x4204
#endif
#ifndef PTRACE_SETREGSET
#define PTRACE_SETREGSET 0x4205
#endif

#define FIRST_PRIMARY 0x13579bdf2468ace0UL
#define FIRST_TLS 0x0fedcba987654320UL
#define SECOND_PRIMARY 0x2468ace013579bdfUL
#define SECOND_TLS 0x0123456789abcdefUL

struct register_observation {
    unsigned long primary;
    unsigned long tls;
};

#if defined(__riscv) && __riscv_xlen == 64

struct arch_user_regs {
    unsigned long pc;
    unsigned long ra;
    unsigned long sp;
    unsigned long gp;
    unsigned long tp;
    unsigned long remaining[27];
};

static unsigned long user_regs_primary(const struct arch_user_regs *regs)
{
    return regs->gp;
}

static unsigned long user_regs_tls(const struct arch_user_regs *regs)
{
    return regs->tp;
}

static void set_user_regs(struct arch_user_regs *regs, unsigned long primary,
                          unsigned long tls)
{
    regs->gp = primary;
    regs->tp = tls;
}

#define ARCH_PRIMARY_NAME "gp"
#define ARCH_TLS_NAME "tp"
#define ARCH_A0 "a0"
#define ARCH_A1 "a1"
#define ARCH_A2 "a2"
#define ARCH_A3 "a3"
#define ARCH_A4 "a4"
#define ARCH_A7 "a7"
#define ARCH_HANDLER_TARGET "s2"
#define ARCH_SYSCALL_INSN "ecall\n"
#define ARCH_SAVE_PRIMARY "mv %[original_primary], gp\n"
#define ARCH_SAVE_TLS "mv %[original_tls], tp\n"
#define ARCH_SET_PRIMARY "mv gp, %[requested_primary]\n"
#define ARCH_SET_TLS "mv tp, %[requested_tls]\n"
#define ARCH_READ_PRIMARY "mv %[observed_primary], gp\n"
#define ARCH_READ_TLS "mv %[observed_tls], tp\n"
#define ARCH_RESTORE_PRIMARY "mv gp, %[original_primary]\n"
#define ARCH_RESTORE_TLS "mv tp, %[original_tls]\n"

#elif defined(__loongarch__) || defined(__loongarch64)

struct arch_user_regs {
    unsigned long regs[32];
    unsigned long orig_a0;
    unsigned long csr_era;
    unsigned long csr_badv;
    unsigned long reserved[10];
};

_Static_assert(sizeof(struct arch_user_regs) == 45 * sizeof(unsigned long),
               "LoongArch NT_PRSTATUS layout must match Starry/Linux");

static unsigned long user_regs_primary(const struct arch_user_regs *regs)
{
    return regs->regs[21];
}

static unsigned long user_regs_tls(const struct arch_user_regs *regs)
{
    return regs->regs[2];
}

static void set_user_regs(struct arch_user_regs *regs, unsigned long primary,
                          unsigned long tls)
{
    regs->regs[21] = primary;
    regs->regs[2] = tls;
}

#define ARCH_PRIMARY_NAME "r21"
#define ARCH_TLS_NAME "tp"
#define ARCH_A0 "$a0"
#define ARCH_A1 "$a1"
#define ARCH_A2 "$a2"
#define ARCH_A3 "$a3"
#define ARCH_A4 "$a4"
#define ARCH_A7 "$a7"
#define ARCH_HANDLER_TARGET "$s2"
#define ARCH_SYSCALL_INSN "syscall 0\n"
#define ARCH_SAVE_PRIMARY "move %[original_primary], $r21\n"
#define ARCH_SAVE_TLS "move %[original_tls], $tp\n"
#define ARCH_SET_PRIMARY "move $r21, %[requested_primary]\n"
#define ARCH_SET_TLS "move $tp, %[requested_tls]\n"
#define ARCH_READ_PRIMARY "move %[observed_primary], $r21\n"
#define ARCH_READ_TLS "move %[observed_tls], $tp\n"
#define ARCH_RESTORE_PRIMARY "move $r21, %[original_primary]\n"
#define ARCH_RESTORE_TLS "move $tp, %[original_tls]\n"

#else
#error "test-user-register-roundtrip requires riscv64 or loongarch64"
#endif

extern void register_signal_handler(int signo);

static int failures;

static void check_observation(const char *path,
                              const struct register_observation *observed,
                              unsigned long expected_primary,
                              unsigned long expected_tls)
{
    if (observed->primary == expected_primary && observed->tls == expected_tls) {
        printf("PASS: %s preserved user %s=%#lx %s=%#lx\n", path,
               ARCH_PRIMARY_NAME, observed->primary, ARCH_TLS_NAME, observed->tls);
        return;
    }
    printf("FAIL: %s observed user %s=%#lx expected=%#lx %s=%#lx expected=%#lx\n",
           path, ARCH_PRIMARY_NAME, observed->primary, expected_primary,
           ARCH_TLS_NAME, observed->tls, expected_tls);
    failures++;
}

static long raw_getpid_probe(struct register_observation *after)
{
    unsigned long original_primary;
    unsigned long original_tls;
    unsigned long observed_primary;
    unsigned long observed_tls;
    register long result __asm__(ARCH_A0);
    register long syscall_number __asm__(ARCH_A7) = SYS_getpid;

    __asm__ volatile(
        ARCH_SAVE_PRIMARY
        ARCH_SAVE_TLS
        ARCH_SET_PRIMARY
        ARCH_SET_TLS
        ARCH_SYSCALL_INSN
        ARCH_READ_PRIMARY
        ARCH_READ_TLS
        ARCH_RESTORE_PRIMARY
        ARCH_RESTORE_TLS
        : [original_primary] "=&r"(original_primary),
          [original_tls] "=&r"(original_tls),
          [observed_primary] "=&r"(observed_primary),
          [observed_tls] "=&r"(observed_tls), [result] "=r"(result)
        : [requested_primary] "r"(FIRST_PRIMARY),
          [requested_tls] "r"(FIRST_TLS), [syscall_number] "r"(syscall_number)
        : "memory");

    after->primary = observed_primary;
    after->tls = observed_tls;
    return result;
}

static long raw_clone_probe(struct register_observation *after)
{
    unsigned long original_primary;
    unsigned long original_tls;
    unsigned long observed_primary;
    unsigned long observed_tls;
    register long result __asm__(ARCH_A0) = SIGCHLD;
    register long stack __asm__(ARCH_A1) = 0;
    register long parent_tid __asm__(ARCH_A2) = 0;
    register long child_tls __asm__(ARCH_A3) = 0;
    register long child_tid __asm__(ARCH_A4) = 0;
    register long syscall_number __asm__(ARCH_A7) = SYS_clone;

    __asm__ volatile(
        ARCH_SAVE_PRIMARY
        ARCH_SAVE_TLS
        ARCH_SET_PRIMARY
        ARCH_SET_TLS
        ARCH_SYSCALL_INSN
        ARCH_READ_PRIMARY
        ARCH_READ_TLS
        ARCH_RESTORE_PRIMARY
        ARCH_RESTORE_TLS
        : [original_primary] "=&r"(original_primary),
          [original_tls] "=&r"(original_tls),
          [observed_primary] "=&r"(observed_primary),
          [observed_tls] "=&r"(observed_tls), [result] "+r"(result)
        : [requested_primary] "r"(FIRST_PRIMARY),
          [requested_tls] "r"(FIRST_TLS), [stack] "r"(stack),
          [parent_tid] "r"(parent_tid), [child_tls] "r"(child_tls),
          [child_tid] "r"(child_tid), [syscall_number] "r"(syscall_number)
        : "memory");

    after->primary = observed_primary;
    after->tls = observed_tls;
    return result;
}

static long raw_signal_probe(pid_t process, pid_t thread,
                             struct register_observation *during,
                             struct register_observation *after, int signal)
{
    unsigned long original_primary;
    unsigned long original_tls;
    unsigned long observed_primary;
    unsigned long observed_tls;
    register long handler_target __asm__(ARCH_HANDLER_TARGET) = (long)during;
    register long result __asm__(ARCH_A0) = process;
    register long target_thread __asm__(ARCH_A1) = thread;
    register long signal_number __asm__(ARCH_A2) = signal;
    register long syscall_number __asm__(ARCH_A7) = SYS_tgkill;

    __asm__ volatile(
        ARCH_SAVE_PRIMARY
        ARCH_SAVE_TLS
        ARCH_SET_PRIMARY
        ARCH_SET_TLS
        ARCH_SYSCALL_INSN
        ARCH_READ_PRIMARY
        ARCH_READ_TLS
        ARCH_RESTORE_PRIMARY
        ARCH_RESTORE_TLS
        : [original_primary] "=&r"(original_primary),
          [original_tls] "=&r"(original_tls),
          [observed_primary] "=&r"(observed_primary),
          [observed_tls] "=&r"(observed_tls), [result] "+r"(result)
        : [requested_primary] "r"(FIRST_PRIMARY),
          [requested_tls] "r"(FIRST_TLS), [handler_target] "r"(handler_target),
          [target_thread] "r"(target_thread), [signal_number] "r"(signal_number),
          [syscall_number] "r"(syscall_number)
        : "memory");

    after->primary = observed_primary;
    after->tls = observed_tls;
    return result;
}

static int test_syscall_round_trip(void)
{
    struct register_observation after = {0};
    long result = raw_getpid_probe(&after);
    if (result != getpid()) {
        printf("FAIL: raw getpid returned %ld errno=%d (%s)\n", result, errno,
               strerror(errno));
        failures++;
    }
    check_observation("syscall return", &after, FIRST_PRIMARY, FIRST_TLS);
    return 0;
}

static int test_clone_round_trip(void)
{
    struct register_observation after = {0};
    long child = raw_clone_probe(&after);
    if (child == 0) {
        _exit(after.primary == FIRST_PRIMARY && after.tls == FIRST_TLS ? 0 : 81);
    }
    if (child < 0) {
        printf("FAIL: raw clone returned %ld errno=%d (%s)\n", child, errno,
               strerror(errno));
        failures++;
        return -1;
    }

    check_observation("clone parent return", &after, FIRST_PRIMARY, FIRST_TLS);
    int status = 0;
    if (waitpid((pid_t)child, &status, 0) != child || !WIFEXITED(status)
        || WEXITSTATUS(status) != 0) {
        printf("FAIL: cloned child register check status=%#x\n", status);
        failures++;
    } else {
        puts("PASS: clone child inherited and restored user-owned registers");
    }
    return 0;
}

static int test_signal_frame_round_trip(void)
{
    struct sigaction action;
    memset(&action, 0, sizeof(action));
    action.sa_handler = register_signal_handler;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGUSR1, &action, NULL) != 0) {
        printf("FAIL: sigaction errno=%d (%s)\n", errno, strerror(errno));
        failures++;
        return -1;
    }

    struct register_observation during = {0};
    struct register_observation after = {0};
    pid_t process = getpid();
    pid_t thread = (pid_t)syscall(SYS_gettid);
    long result = raw_signal_probe(process, thread, &during, &after, SIGUSR1);
    if (result != 0) {
        printf("FAIL: tgkill(SIGUSR1) returned %ld errno=%d (%s)\n", result, errno,
               strerror(errno));
        failures++;
    }
    check_observation("signal handler entry", &during, FIRST_PRIMARY, FIRST_TLS);
    check_observation("rt_sigreturn", &after, FIRST_PRIMARY, FIRST_TLS);
    return 0;
}

static int test_ptrace_signal_stop_round_trip(void)
{
    pid_t child = fork();
    if (child < 0) {
        printf("FAIL: ptrace fork errno=%d (%s)\n", errno, strerror(errno));
        failures++;
        return -1;
    }
    if (child == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(91);
        }
        if (raise(SIGSTOP) != 0) {
            _exit(92);
        }
        struct register_observation unused_during = {0};
        struct register_observation after = {0};
        pid_t process = getpid();
        pid_t thread = (pid_t)syscall(SYS_gettid);
        long result = raw_signal_probe(process, thread, &unused_during, &after, SIGSTOP);
        _exit(result == 0 && after.primary == SECOND_PRIMARY && after.tls == SECOND_TLS
                  ? 0
                  : 93);
    }

    int status = 0;
    if (waitpid(child, &status, 0) != child || !WIFSTOPPED(status)
        || WSTOPSIG(status) != SIGSTOP) {
        printf("FAIL: ptrace initial stop status=%#x\n", status);
        failures++;
        return -1;
    }
    if (ptrace(PTRACE_CONT, child, NULL, NULL) != 0) {
        printf("FAIL: ptrace initial continue errno=%d (%s)\n", errno,
               strerror(errno));
        failures++;
        return -1;
    }
    if (waitpid(child, &status, 0) != child || !WIFSTOPPED(status)
        || WSTOPSIG(status) != SIGSTOP) {
        printf("FAIL: ptrace register stop status=%#x\n", status);
        failures++;
        return -1;
    }

    struct arch_user_regs regs;
    memset(&regs, 0, sizeof(regs));
    struct iovec iov = {.iov_base = &regs, .iov_len = sizeof(regs)};
    if (ptrace(PTRACE_GETREGSET, child, (void *)NT_PRSTATUS, &iov) != 0) {
        printf("FAIL: ptrace getregset errno=%d (%s)\n", errno, strerror(errno));
        failures++;
        return -1;
    }
    struct register_observation stopped = {
        .primary = user_regs_primary(&regs),
        .tls = user_regs_tls(&regs),
    };
    check_observation("ptrace signal stop", &stopped, FIRST_PRIMARY, FIRST_TLS);

    set_user_regs(&regs, SECOND_PRIMARY, SECOND_TLS);
    iov.iov_len = sizeof(regs);
    if (ptrace(PTRACE_SETREGSET, child, (void *)NT_PRSTATUS, &iov) != 0) {
        printf("FAIL: ptrace setregset errno=%d (%s)\n", errno, strerror(errno));
        failures++;
        return -1;
    }
    if (ptrace(PTRACE_CONT, child, NULL, NULL) != 0) {
        printf("FAIL: ptrace final continue errno=%d (%s)\n", errno, strerror(errno));
        failures++;
        return -1;
    }
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status)
        || WEXITSTATUS(status) != 0) {
        printf("FAIL: ptrace child register restore status=%#x\n", status);
        failures++;
    } else {
        puts("PASS: ptrace GETREGSET/SETREGSET round-tripped user-owned registers");
    }
    return 0;
}

int main(void)
{
    puts("TEST: user-owned register round-trip");
    test_syscall_round_trip();
    test_clone_round_trip();
    test_signal_frame_round_trip();
    test_ptrace_signal_stop_round_trip();
    printf("DONE: %s (%d failure%s)\n", failures == 0 ? "pass" : "fail", failures,
           failures == 1 ? "" : "s");
    return failures == 0 ? 0 : 1;
}
