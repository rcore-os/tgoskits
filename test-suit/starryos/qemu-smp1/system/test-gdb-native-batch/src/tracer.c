/*
 * test-gdb-native-batch (raw-ptrace edition)
 *
 * Mirrors the riscv `test-ptrace-gdb` approach: a self-contained C tracer that
 * drives ptrace(2) directly instead of depending on the real /usr/bin/gdb.
 *
 * It debugs the *dynamically linked* target `test-gdb-native-batch-target`
 * across execve() and the full ld-musl startup -- the exact scenario the old
 * real-GDB test was meant to cover -- using deterministic ptrace operations:
 *   1. fork + PTRACE_TRACEME + execve(dynamic target)  -> exec-stop
 *   2. read AT_ENTRY from /proc/<pid>/auxv (the relocated application entry)
 *   3. plant an INT3 at AT_ENTRY, PTRACE_CONT: ld-musl runs to completion under
 *      ptrace and traps at the application entry (proves the dynamic linker is
 *      not corrupted while traced)
 *   4. restore the byte, single-step one instruction (validates TF/#DB)
 *   5. PTRACE_CONT to exit and check the exit code
 */
#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ptrace.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef PTRACE_TRACEME
#define PTRACE_TRACEME 0
#endif
#ifndef PTRACE_PEEKTEXT
#define PTRACE_PEEKTEXT 1
#endif
#ifndef PTRACE_POKETEXT
#define PTRACE_POKETEXT 4
#endif
#ifndef PTRACE_CONT
#define PTRACE_CONT 7
#endif
#ifndef PTRACE_KILL
#define PTRACE_KILL 8
#endif
#ifndef PTRACE_SINGLESTEP
#define PTRACE_SINGLESTEP 9
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
#ifndef AT_ENTRY
#define AT_ENTRY 9
#endif
#ifndef AT_NULL
#define AT_NULL 0
#endif

extern char **environ;

/* amd64 user_regs_struct layout (matches kernel X8664UserRegs). */
struct x86_64_user_regs {
    uint64_t r15, r14, r13, r12, rbp, rbx, r11, r10, r9, r8;
    uint64_t rax, rcx, rdx, rsi, rdi, orig_rax, rip, cs, eflags, rsp, ss;
    uint64_t fs_base, gs_base, ds, es, fs, gs;
};

static int fail(const char *msg)
{
    printf("FAIL: %s: errno=%d (%s)\n", msg, errno, strerror(errno));
    return 1;
}

static long pt(int request, pid_t pid, void *addr, void *data)
{
    return ptrace(request, pid, addr, data);
}

static int getregs(pid_t pid, struct x86_64_user_regs *regs)
{
    struct iovec iov = {.iov_base = regs, .iov_len = sizeof(*regs)};
    if (pt(PTRACE_GETREGSET, pid, (void *)(long)NT_PRSTATUS, &iov) != 0) {
        return -1;
    }
    return 0;
}

static int setregs(pid_t pid, const struct x86_64_user_regs *regs)
{
    struct iovec iov = {.iov_base = (void *)regs, .iov_len = sizeof(*regs)};
    return pt(PTRACE_SETREGSET, pid, (void *)(long)NT_PRSTATUS, &iov) == 0 ? 0 : -1;
}

static int wait_trap(pid_t pid, int *status)
{
    if (waitpid(pid, status, 0) != pid) {
        return fail("waitpid");
    }
    if (!WIFSTOPPED(*status) || WSTOPSIG(*status) != SIGTRAP) {
        printf("FAIL: expected SIGTRAP stop, status=%#x\n", *status);
        return 1;
    }
    return 0;
}

/* Read the relocated application entry (AT_ENTRY) from /proc/<pid>/auxv. */
static unsigned long read_at_entry(pid_t pid)
{
    char path[64];
    snprintf(path, sizeof(path), "/proc/%d/auxv", pid);
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        return 0;
    }
    unsigned long pair[2];
    unsigned long entry = 0;
    ssize_t n;
    while ((n = read(fd, pair, sizeof(pair))) == (ssize_t)sizeof(pair)) {
        if (pair[0] == AT_NULL) {
            break;
        }
        if (pair[0] == AT_ENTRY) {
            entry = pair[1];
            break;
        }
    }
    close(fd);
    return entry;
}

static int trace_dynamic_target(void)
{
    static const char *target = "/usr/bin/test-gdb-native-batch-target";

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork");
    }
    if (pid == 0) {
        if (pt(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(100);
        }
        char *const argv[] = {(char *)target, NULL};
        execve(target, argv, environ);
        _exit(101);
    }

    /* 1. exec-stop: TRACEME + execve of a dynamic binary delivers SIGTRAP. */
    int status = 0;
    if (wait_trap(pid, &status) != 0) {
        return 1;
    }
    printf("  ok: exec-stop on dynamic target\n");

    /* 2. AT_ENTRY = relocated application entry, reached only after ld-musl. */
    unsigned long at_entry = read_at_entry(pid);
    if (at_entry == 0) {
        printf("FAIL: could not read AT_ENTRY from /proc/%d/auxv\n", pid);
        return 1;
    }
    printf("  ok: AT_ENTRY=%#lx\n", at_entry);

    struct x86_64_user_regs regs;
    memset(&regs, 0, sizeof(regs));
    if (getregs(pid, &regs) != 0) {
        return fail("getregs at exec-stop");
    }
    if (regs.cs == 0 || regs.ss == 0) {
        printf("FAIL: bad user selectors cs=%#llx ss=%#llx\n",
               (unsigned long long)regs.cs, (unsigned long long)regs.ss);
        return 1;
    }
    printf("  ok: user selectors cs=%#llx ss=%#llx\n",
           (unsigned long long)regs.cs, (unsigned long long)regs.ss);

    /* 3. Plant INT3 at AT_ENTRY and continue: ld-musl runs fully under ptrace. */
    errno = 0;
    long orig = pt(PTRACE_PEEKTEXT, pid, (void *)at_entry, NULL);
    if (orig == -1 && errno != 0) {
        return fail("peektext at AT_ENTRY");
    }
    long bp = (orig & ~0xffL) | 0xcc;
    if (pt(PTRACE_POKETEXT, pid, (void *)at_entry, (void *)bp) != 0) {
        return fail("poketext int3 at AT_ENTRY");
    }
    if (pt(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("cont to AT_ENTRY breakpoint");
    }
    if (wait_trap(pid, &status) != 0) {
        return 1;
    }
    if (getregs(pid, &regs) != 0) {
        return fail("getregs at breakpoint");
    }
    /* x86 reports rip just past the int3 (kernel does not decrement). */
    if (regs.rip != at_entry + 1) {
        printf("FAIL: breakpoint rip=%#llx, expected AT_ENTRY+1=%#lx\n",
               (unsigned long long)regs.rip, at_entry + 1);
        return 1;
    }
    printf("  ok: ld-musl startup ran under ptrace, trapped at application entry\n");

    /* 4. Restore the byte, back up rip, single-step one real instruction. */
    if (pt(PTRACE_POKETEXT, pid, (void *)at_entry, (void *)orig) != 0) {
        return fail("restore original byte");
    }
    regs.rip = at_entry;
    if (setregs(pid, &regs) != 0) {
        return fail("setregs rip back to AT_ENTRY");
    }
    if (pt(PTRACE_SINGLESTEP, pid, NULL, NULL) != 0) {
        return fail("singlestep at application entry");
    }
    if (wait_trap(pid, &status) != 0) {
        return 1;
    }
    if (getregs(pid, &regs) != 0) {
        return fail("getregs after singlestep");
    }
    if (regs.rip == at_entry) {
        printf("FAIL: single-step did not advance rip past AT_ENTRY=%#lx\n", at_entry);
        return 1;
    }
    printf("  ok: single-step advanced rip to %#llx\n", (unsigned long long)regs.rip);

    /* 5. Continue to exit; the target prints value=42 and exits 0. */
    if (pt(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("cont to exit");
    }
    if (waitpid(pid, &status, 0) != pid) {
        return fail("waitpid exit");
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        printf("FAIL: traced dynamic target did not exit cleanly, status=%#x\n", status);
        if (WIFSIGNALED(status)) {
            pt(PTRACE_KILL, pid, NULL, NULL);
        }
        return 1;
    }
    printf("  ok: traced dynamic target exited 0\n");
    return 0;
}

int main(void)
{
    int pass = 0;
    int fail_count = 0;

    printf("test: raw-ptrace debug of dynamic target across execve + ld-musl\n");
    if (trace_dynamic_target() == 0) {
        pass++;
    } else {
        fail_count++;
    }

    printf("DONE: %d pass, %d fail\n", pass, fail_count);
    return fail_count > 0 ? 1 : 0;
}
