#define _GNU_SOURCE

#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ptrace.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef PTRACE_GETREGS
#define PTRACE_GETREGS 12
#endif
#ifndef PTRACE_TRACEME
#define PTRACE_TRACEME 0
#endif
#ifndef PTRACE_SETREGS
#define PTRACE_SETREGS 13
#endif
#ifndef PTRACE_CONT
#define PTRACE_CONT 7
#endif
#ifndef PTRACE_GETSIGINFO
#define PTRACE_GETSIGINFO 0x4202
#endif
#ifndef PTRACE_GETREGSET
#define PTRACE_GETREGSET 0x4204
#endif
#ifndef PTRACE_SETREGSET
#define PTRACE_SETREGSET 0x4205
#endif
#ifndef NT_PRSTATUS
#define NT_PRSTATUS 1
#endif

struct x86_64_user_regs {
    uint64_t r15;
    uint64_t r14;
    uint64_t r13;
    uint64_t r12;
    uint64_t rbp;
    uint64_t rbx;
    uint64_t r11;
    uint64_t r10;
    uint64_t r9;
    uint64_t r8;
    uint64_t rax;
    uint64_t rcx;
    uint64_t rdx;
    uint64_t rsi;
    uint64_t rdi;
    uint64_t orig_rax;
    uint64_t rip;
    uint64_t cs;
    uint64_t eflags;
    uint64_t rsp;
    uint64_t ss;
    uint64_t fs_base;
    uint64_t gs_base;
    uint64_t ds;
    uint64_t es;
    uint64_t fs;
    uint64_t gs;
};

static long ptrace_call(int request, pid_t pid, void *addr, void *data)
{
#ifdef __APPLE__
    return ptrace(request, pid, (caddr_t)addr, (int)(intptr_t)data);
#else
    return ptrace(request, pid, addr, data);
#endif
}

static int fail_errno(const char *msg)
{
    printf("FAIL: %s: errno=%d (%s)\n", msg, errno, strerror(errno));
    return 1;
}

static int fail_msg(const char *msg)
{
    printf("FAIL: %s\n", msg);
    return 1;
}

static int getregs(pid_t pid, struct x86_64_user_regs *regs)
{
    if (ptrace_call(PTRACE_GETREGS, pid, NULL, regs) != 0) {
        return -1;
    }
    return 0;
}

static int setregs(pid_t pid, const struct x86_64_user_regs *regs)
{
    if (ptrace_call(PTRACE_SETREGS, pid, NULL, (void *)regs) != 0) {
        return -1;
    }
    return 0;
}

static int getregset(pid_t pid, struct x86_64_user_regs *regs)
{
    struct iovec iov = {.iov_base = regs, .iov_len = sizeof(*regs)};
    if (ptrace_call(PTRACE_GETREGSET, pid, (void *)NT_PRSTATUS, &iov) != 0) {
        return -1;
    }
    if (iov.iov_len != (long)sizeof(*regs)) {
        errno = EIO;
        return -1;
    }
    return 0;
}

static int setregset(pid_t pid, const struct x86_64_user_regs *regs)
{
    struct iovec iov = {.iov_base = (void *)regs, .iov_len = sizeof(*regs)};
    if (ptrace_call(PTRACE_SETREGSET, pid, (void *)NT_PRSTATUS, &iov) != 0) {
        return -1;
    }
    return 0;
}

static int test_ptrace_x86_regs(void)
{
    printf("test 1: x86_64 ptrace regs/siginfo\n");

    pid_t pid = fork();
    if (pid < 0) {
        return fail_errno("fork");
    }

    if (pid == 0) {
        if (ptrace_call(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(100);
        }
        raise(SIGSTOP);
        _exit(0);
    }

    int status = 0;
    if (waitpid(pid, &status, WUNTRACED) != pid) {
        return fail_errno("waitpid stop");
    }
    if (!WIFSTOPPED(status) || WSTOPSIG(status) != SIGSTOP) {
        printf("FAIL: expected SIGSTOP stop, status=%#x\n", status);
        return 1;
    }
    printf("  ok: child stopped with SIGSTOP\n");

    siginfo_t si;
    memset(&si, 0, sizeof(si));
    if (ptrace_call(PTRACE_GETSIGINFO, pid, NULL, &si) != 0) {
        return fail_errno("PTRACE_GETSIGINFO");
    }
    if (si.si_signo != SIGSTOP) {
        printf("FAIL: si_signo=%d, expected %d\n", si.si_signo, SIGSTOP);
        return 1;
    }
    printf("  ok: GETSIGINFO reports SIGSTOP\n");

    struct x86_64_user_regs regs_getregs;
    memset(&regs_getregs, 0, sizeof(regs_getregs));
    if (getregs(pid, &regs_getregs) != 0) {
        return fail_errno("PTRACE_GETREGS");
    }

    struct x86_64_user_regs regs_getregset;
    memset(&regs_getregset, 0, sizeof(regs_getregset));
    if (getregset(pid, &regs_getregset) != 0) {
        return fail_errno("PTRACE_GETREGSET");
    }

    if (regs_getregs.rip == 0 || regs_getregs.rsp == 0) {
        return fail_msg("GETREGS returned zero RIP/RSP");
    }
    if (regs_getregs.rip != regs_getregset.rip ||
        regs_getregs.rsp != regs_getregset.rsp ||
        regs_getregs.rax != regs_getregset.rax ||
        regs_getregs.r15 != regs_getregset.r15) {
        printf("FAIL: GETREGS/GETREGSET mismatch: rip=%#llx/%#llx rsp=%#llx/%#llx rax=%#llx/%#llx r15=%#llx/%#llx\n",
               (unsigned long long)regs_getregs.rip,
               (unsigned long long)regs_getregset.rip,
               (unsigned long long)regs_getregs.rsp,
               (unsigned long long)regs_getregset.rsp,
               (unsigned long long)regs_getregs.rax,
               (unsigned long long)regs_getregset.rax,
               (unsigned long long)regs_getregs.r15,
               (unsigned long long)regs_getregset.r15);
        return 1;
    }
    printf("  ok: GETREGS and GETREGSET agree on core registers\n");

    struct x86_64_user_regs modified = regs_getregset;
    modified.r15 ^= 0x13579bdfULL;
    if (setregset(pid, &modified) != 0) {
        return fail_errno("PTRACE_SETREGSET");
    }

    struct x86_64_user_regs check_after_setregset;
    memset(&check_after_setregset, 0, sizeof(check_after_setregset));
    if (getregs(pid, &check_after_setregset) != 0) {
        return fail_errno("GETREGS after SETREGSET");
    }
    if (check_after_setregset.r15 != modified.r15) {
        printf("FAIL: SETREGSET did not update r15 (%#llx != %#llx)\n",
               (unsigned long long)check_after_setregset.r15,
               (unsigned long long)modified.r15);
        return 1;
    }
    printf("  ok: SETREGSET updated r15\n");

    modified = check_after_setregset;
    modified.r14 ^= 0x2468ace0ULL;
    if (setregs(pid, &modified) != 0) {
        return fail_errno("PTRACE_SETREGS");
    }

    struct x86_64_user_regs check_after_setregs;
    memset(&check_after_setregs, 0, sizeof(check_after_setregs));
    if (getregset(pid, &check_after_setregs) != 0) {
        return fail_errno("GETREGSET after SETREGS");
    }
    if (check_after_setregs.r14 != modified.r14) {
        printf("FAIL: SETREGS did not update r14 (%#llx != %#llx)\n",
               (unsigned long long)check_after_setregs.r14,
               (unsigned long long)modified.r14);
        return 1;
    }
    printf("  ok: SETREGS updated r14\n");

    if (setregs(pid, &regs_getregs) != 0) {
        return fail_errno("restore original regs");
    }

    if (ptrace_call(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail_errno("PTRACE_CONT");
    }

    if (waitpid(pid, &status, 0) != pid) {
        return fail_errno("waitpid exit");
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        printf("FAIL: expected child exit 0, status=%#x\n", status);
        return 1;
    }
    printf("  ok: child continued and exited cleanly\n");
    return 0;
}

int main(void)
{
    int pass = 0;
    int fail = 0;

    if (test_ptrace_x86_regs() == 0) {
        pass++;
    } else {
        fail++;
    }

    printf("DONE: %d pass, %d fail\n", pass, fail);
    return fail == 0 ? 0 : 1;
}
