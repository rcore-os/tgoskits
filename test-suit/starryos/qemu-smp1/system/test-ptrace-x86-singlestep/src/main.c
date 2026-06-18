#define _GNU_SOURCE

#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ptrace.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef PTRACE_GETREGSET
#define PTRACE_GETREGSET 0x4204
#endif
#ifndef PTRACE_SETREGSET
#define PTRACE_SETREGSET 0x4205
#endif
#ifndef PTRACE_TRACEME
#define PTRACE_TRACEME 0
#endif
#ifndef PTRACE_SINGLESTEP
#define PTRACE_SINGLESTEP 9
#endif
#ifndef PTRACE_CONT
#define PTRACE_CONT 7
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

__attribute__((naked, noinline, aligned(1))) static void singlestep_target(void)
{
    __asm__ volatile(
        "mov $123, %rax\n"
        "int3\n"
        "jmp singlestep_landing\n");
}

__attribute__((naked, noinline, aligned(1), used)) static void singlestep_landing(void)
{
    __asm__ volatile(
        "mov $42, %rdi\n"
        "mov $60, %rax\n"
        "syscall\n");
}

static int fail(const char *msg)
{
    printf("FAIL: %s: errno=%d (%s)\n", msg, errno, strerror(errno));
    return 1;
}

static long ptrace_call(int request, pid_t pid, void *addr, void *data)
{
#ifdef __APPLE__
    return ptrace(request, pid, (caddr_t)addr, (int)(intptr_t)data);
#else
    return ptrace(request, pid, addr, data);
#endif
}

static int wait_stopped(pid_t pid, int *status, int expected_sig)
{
    if (waitpid(pid, status, WUNTRACED) != pid) {
        return fail("waitpid");
    }
    if (!WIFSTOPPED(*status) || WSTOPSIG(*status) != expected_sig) {
        printf("FAIL: expected stop signal %d, status=%#x\n", expected_sig, *status);
        return 1;
    }
    return 0;
}

static int getregset(pid_t pid, struct x86_64_user_regs *regs)
{
    struct iovec iov = {.iov_base = regs, .iov_len = sizeof(*regs)};
    if (ptrace_call(PTRACE_GETREGSET, pid, (void *)(long)NT_PRSTATUS, &iov) != 0) {
        return -1;
    }
    if ((size_t)iov.iov_len != sizeof(*regs)) {
        errno = EIO;
        return -1;
    }
    return 0;
}

static int setregset(pid_t pid, const struct x86_64_user_regs *regs)
{
    struct iovec iov = {.iov_base = (void *)regs, .iov_len = sizeof(*regs)};
    if (ptrace_call(PTRACE_SETREGSET, pid, (void *)(long)NT_PRSTATUS, &iov) != 0) {
        return -1;
    }
    return 0;
}

static int test_singlestep(void)
{
    printf("test 1: x86_64 PTRACE_SINGLESTEP\n");

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork");
    }

    if (pid == 0) {
        if (ptrace_call(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(100);
        }
        raise(SIGSTOP);
        _exit(99);
    }

    int status = 0;
    if (wait_stopped(pid, &status, SIGSTOP) != 0) {
        return 1;
    }
    printf("  ok: child stopped with SIGSTOP\n");

    struct x86_64_user_regs regs;
    memset(&regs, 0, sizeof(regs));
    if (getregset(pid, &regs) != 0) {
        return fail("getregset initial");
    }

    regs.rip = (uintptr_t)singlestep_target;
    regs.rax = 0;
    if (setregset(pid, &regs) != 0) {
        return fail("setregset target rip");
    }

    if (ptrace_call(PTRACE_SINGLESTEP, pid, NULL, NULL) != 0) {
        return fail("ptrace singlestep");
    }
    if (wait_stopped(pid, &status, SIGTRAP) != 0) {
        return 1;
    }
    printf("  ok: single-step trap delivered\n");

    memset(&regs, 0, sizeof(regs));
    if (getregset(pid, &regs) != 0) {
        return fail("getregset after singlestep");
    }
    if (regs.rax != 123) {
        printf("FAIL: expected RAX=123 after single-step, got %#llx\n",
               (unsigned long long)regs.rax);
        return 1;
    }
    if (regs.rip == (uintptr_t)singlestep_target) {
        printf("FAIL: RIP did not advance after single-step: %#llx\n",
               (unsigned long long)regs.rip);
        return 1;
    }
    printf("  ok: single-step executed one instruction (RAX=%#llx RIP=%#llx)\n",
           (unsigned long long)regs.rax, (unsigned long long)regs.rip);

    if (ptrace_call(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("ptrace cont to int3");
    }
    if (wait_stopped(pid, &status, SIGTRAP) != 0) {
        return 1;
    }
    printf("  ok: child reached the following int3\n");

    if (ptrace_call(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("ptrace cont to exit");
    }
    if (waitpid(pid, &status, 0) != pid) {
        return fail("waitpid exit");
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 42) {
        printf("FAIL: expected exit 42, status=%#x\n", status);
        return 1;
    }
    printf("  ok: child exited with 42\n");
    return 0;
}

int main(void)
{
    int pass = 0;
    int fail_count = 0;

    if (test_singlestep() == 0) {
        pass++;
    } else {
        fail_count++;
    }

    printf("DONE: %d pass, %d fail\n", pass, fail_count);
    return fail_count > 0 ? 1 : 0;
}
