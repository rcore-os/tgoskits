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
#ifndef PTRACE_PEEKDATA
#define PTRACE_PEEKDATA 2
#endif
#ifndef PTRACE_POKEDATA
#define PTRACE_POKEDATA 5
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

__attribute__((noinline)) static int native_marker(int value)
{
    return value + 1;
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

static int test_breakpoint_reinsert(void)
{
    printf("test 1: x86_64 breakpoint restore + singlestep + reinsert\n");

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork");
    }

    if (pid == 0) {
        if (ptrace_call(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(100);
        }
        raise(SIGSTOP);
        int value = native_marker(40);
        value = native_marker(value);
        _exit(value == 42 ? 42 : 1);
    }

    int status = 0;
    if (wait_stopped(pid, &status, SIGSTOP) != 0) {
        return 1;
    }
    printf("  ok: child stopped with SIGSTOP\n");

    uintptr_t bp_addr = (uintptr_t)native_marker;
    errno = 0;
    long orig_word = ptrace_call(PTRACE_PEEKDATA, pid, (void *)bp_addr, NULL);
    if (orig_word == -1 && errno != 0) {
        return fail("PTRACE_PEEKDATA original instruction");
    }
    unsigned long bp_word = ((unsigned long)orig_word & ~0xffUL) | 0xccUL;
    if (ptrace_call(PTRACE_POKEDATA, pid, (void *)bp_addr, (void *)bp_word) != 0) {
        return fail("PTRACE_POKEDATA install breakpoint");
    }
    printf("  ok: installed int3 at native_marker\n");

    if (ptrace_call(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("PTRACE_CONT first call");
    }
    if (wait_stopped(pid, &status, SIGTRAP) != 0) {
        return 1;
    }
    printf("  ok: first breakpoint hit\n");

    struct x86_64_user_regs regs;
    memset(&regs, 0, sizeof(regs));
    if (getregset(pid, &regs) != 0) {
        return fail("getregset at first breakpoint");
    }
    if (regs.rip != bp_addr + 1 && regs.rip != bp_addr) {
        printf("FAIL: RIP=%#llx not at breakpoint %#llx\n",
               (unsigned long long)regs.rip, (unsigned long long)bp_addr);
        return 1;
    }

    if (ptrace_call(PTRACE_POKEDATA, pid, (void *)bp_addr, (void *)orig_word) != 0) {
        return fail("PTRACE_POKEDATA restore original instruction");
    }
    regs.rip = bp_addr;
    if (setregset(pid, &regs) != 0) {
        return fail("setregset rewind RIP to breakpoint");
    }
    printf("  ok: restored original byte and rewound RIP\n");

    if (ptrace_call(PTRACE_SINGLESTEP, pid, NULL, NULL) != 0) {
        return fail("PTRACE_SINGLESTEP over original instruction");
    }
    if (wait_stopped(pid, &status, SIGTRAP) != 0) {
        return 1;
    }
    printf("  ok: single-step completed original instruction\n");

    memset(&regs, 0, sizeof(regs));
    if (getregset(pid, &regs) != 0) {
        return fail("getregset after singlestep");
    }
    if (regs.rip == bp_addr) {
        printf("FAIL: RIP did not advance after single-step: %#llx\n",
               (unsigned long long)regs.rip);
        return 1;
    }

    if (ptrace_call(PTRACE_POKEDATA, pid, (void *)bp_addr, (void *)bp_word) != 0) {
        return fail("PTRACE_POKEDATA reinsert breakpoint");
    }
    printf("  ok: reinserted breakpoint at native_marker\n");

    if (ptrace_call(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("PTRACE_CONT second call");
    }
    if (wait_stopped(pid, &status, SIGTRAP) != 0) {
        return 1;
    }
    printf("  ok: second breakpoint hit after reinsertion\n");

    memset(&regs, 0, sizeof(regs));
    if (getregset(pid, &regs) != 0) {
        return fail("getregset at second breakpoint");
    }
    if (regs.rip != bp_addr + 1 && regs.rip != bp_addr) {
        printf("FAIL: second-hit RIP=%#llx not at breakpoint %#llx\n",
               (unsigned long long)regs.rip, (unsigned long long)bp_addr);
        return 1;
    }

    if (ptrace_call(PTRACE_POKEDATA, pid, (void *)bp_addr, (void *)orig_word) != 0) {
        return fail("PTRACE_POKEDATA restore after second hit");
    }
    regs.rip = bp_addr;
    if (setregset(pid, &regs) != 0) {
        return fail("setregset rewind RIP after second hit");
    }

    if (ptrace_call(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("PTRACE_CONT final exit");
    }
    if (waitpid(pid, &status, 0) != pid) {
        return fail("waitpid final exit");
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 42) {
        printf("FAIL: expected exit 42, status=%#x\n", status);
        return 1;
    }
    printf("  ok: child exited with 42 after breakpoint reinsertion flow\n");
    return 0;
}

int main(void)
{
    int pass = 0;
    int fail_count = 0;

    if (test_breakpoint_reinsert() == 0) {
        pass++;
    } else {
        fail_count++;
    }

    printf("DONE: %d pass, %d fail\n", pass, fail_count);
    return fail_count > 0 ? 1 : 0;
}
