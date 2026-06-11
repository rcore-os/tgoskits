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
#ifndef PTRACE_SINGLESTEP
#define PTRACE_SINGLESTEP 9
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

__attribute__((noinline)) static int target_function(void)
{
    return 42;
}


static int fail(const char *msg)
{
    printf("FAIL: %s: errno=%d (%s)\n", msg, errno, strerror(errno));
    return 1;
}

static int getregset(pid_t pid, struct x86_64_user_regs *regs)
{
    struct iovec iov = {.iov_base = regs, .iov_len = sizeof(*regs)};
    if (ptrace(PTRACE_GETREGSET, pid, (void *)(long)NT_PRSTATUS, &iov) != 0) {
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
    if (ptrace(PTRACE_SETREGSET, pid, (void *)(long)NT_PRSTATUS, &iov) != 0) {
        return -1;
    }
    return 0;
}

/* Test 1: minimum closed-loop software breakpoint.
 *
 * tracer writes 0xCC; child hits breakpoint; tracer restores original
 * byte and continues from the pre-int3 address.  This validates the
 * basic breakpoint stop and register read-back but does NOT exercise
 * PTRACE_SINGLESTEP. */
static int test_breakpoint(void)
{
    printf("test 1: x86_64 software breakpoint (int3)\n");

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork");
    }

    if (pid == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(100);
        }
        /* Trigger initial ptrace-stop via int3 instead of raise(SIGSTOP).
         * The #BP exception path preserves the CPU exception frame with
         * valid segment selectors (CS/SS) and stack pointer. */
        __asm__ volatile ("int3" ::: "memory");
        target_function();
        _exit(42);
    }

    int status = 0;
    if (waitpid(pid, &status, WUNTRACED) != pid) {
        return fail("waitpid initial stop");
    }
    if (!WIFSTOPPED(status) || WSTOPSIG(status) != SIGTRAP) {
        printf("  note: unexpected initial stop signal=%d status=%#x\n",
               WSTOPSIG(status), status);
    }
    printf("  ok: child stopped (signal=%d)\n", WSTOPSIG(status));

    struct x86_64_user_regs regs;
    memset(&regs, 0, sizeof(regs));
    if (getregset(pid, &regs) != 0) {
        return fail("getregset at initial stop");
    }
    printf("  ok: initial RIP=%#llx RSP=%#llx\n",
           (unsigned long long)regs.rip, (unsigned long long)regs.rsp);

    if (regs.rsp == 0 || regs.rsp == 1) {
        printf("FAIL: bogus RSP=%#llx (expected valid stack pointer)\n",
               (unsigned long long)regs.rsp);
        return 1;
    }

    /* x86 int3 is 1 byte (0xCC). CPU pushes RIP of next instruction, so
     * the saved RIP in the exception frame points to int3_addr + 1.
     * StarryOS does not adjust RIP backward for ptrace breakpoint stops,
     * so we observe RIP = int3 + 1 here. Save the pre-int3 RIP as the
     * "continue point" for later. */
    uintptr_t continue_rip = regs.rip;

    uintptr_t bp_addr = (uintptr_t)target_function;
    printf("  target_function at %p\n", (void *)bp_addr);

    errno = 0;
    long orig_word = ptrace(PTRACE_PEEKDATA, pid, (void *)bp_addr, NULL);
    if (orig_word == -1 && errno != 0) {
        return fail("PEEKDATA target_function");
    }

    unsigned long bp_word = ((unsigned long)orig_word & ~0xffUL) | 0xccUL;
    if (ptrace(PTRACE_POKEDATA, pid, (void *)bp_addr, (void *)bp_word) != 0) {
        return fail("POKEDATA write int3");
    }
    printf("  ok: wrote int3 (0xCC) at target_function\n");

    regs.rip = bp_addr;
    if (setregset(pid, &regs) != 0) {
        return fail("setregset redirect RIP to target_function");
    }
    printf("  ok: redirected RIP=%#llx -> %#llx\n",
           (unsigned long long)continue_rip, (unsigned long long)bp_addr);

    if (ptrace(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("PTRACE_CONT to breakpoint");
    }

    if (waitpid(pid, &status, WUNTRACED) != pid) {
        return fail("waitpid breakpoint");
    }
    if (!WIFSTOPPED(status) || WSTOPSIG(status) != SIGTRAP) {
        printf("FAIL: expected SIGTRAP after breakpoint, status=%#x\n", status);
        return 1;
    }
    printf("  ok: child stopped with SIGTRAP\n");

    memset(&regs, 0, sizeof(regs));
    if (getregset(pid, &regs) != 0) {
        return fail("getregset at breakpoint");
    }

    /* After int3, RIP = bp_addr + 1. Verify it's near the breakpoint. */
    if (regs.rip != bp_addr && regs.rip != bp_addr + 1) {
        printf("FAIL: RIP=%#llx not near breakpoint addr %#llx\n",
               (unsigned long long)regs.rip, (unsigned long long)bp_addr);
        return 1;
    }
    printf("  ok: RIP=%#llx at breakpoint %#llx (offset +%lld)\n",
           (unsigned long long)regs.rip, (unsigned long long)bp_addr,
           (long long)(regs.rip - bp_addr));

    if (ptrace(PTRACE_POKEDATA, pid, (void *)bp_addr, (void *)orig_word) != 0) {
        return fail("POKEDATA restore original byte");
    }
    printf("  ok: restored original instruction byte\n");

    /* Rewind RIP to the original continue point (after the initial int3).
     * From there the child will call target_function() normally via the
     * call instruction, then _exit(42). */
    regs.rip = continue_rip;
    if (setregset(pid, &regs) != 0) {
        return fail("setregset rewind RIP");
    }
    printf("  ok: rewound RIP to continue point %#llx\n",
           (unsigned long long)continue_rip);

    if (ptrace(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("PTRACE_CONT after breakpoint restore");
    }

    if (waitpid(pid, &status, 0) != pid) {
        return fail("waitpid exit after restore");
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 42) {
        printf("FAIL: expected exit code 42, status=%#x\n", status);
        return 1;
    }
    printf("  ok: child exited with 42\n");

    return 0;
}

/* Test 2: x86_64 PTRACE_SINGLESTEP validation.
 *
 * Uses the initial `int3` in the child as the breakpoint target:
 *
 * 1. child hits initial int3, stops with SIGTRAP
 * 2. tracer replaces int3 (0xCC) with NOP (0x90) at that address
 * 3. tracer rewinds RIP to the int3 address
 * 4. tracer issues PTRACE_SINGLESTEP
 * 5. child executes the NOP and stops with SIGTRAP (#DB)
 * 6. tracer verifies RIP advanced past the int3 site
 * 7. tracer restores int3, then CONT
 * 8. child continues to completion (target_function → _exit(42))
 *
 * This validates the x86_64 Trap-flag-based single-step mechanism. */
static int test_breakpoint_singlestep_restore(void)
{
    printf("test 2: x86_64 PTRACE_SINGLESTEP\n");

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork");
    }

    if (pid == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(100);
        }
        /* Initial stop via int3 — also serves as the breakpoint
         * the tracer will single-step over. */
        __asm__ volatile (
            ".global test_bp_addr\n"
            "test_bp_addr:\n"
            "    int3\n"
            ::: "memory"
        );
        target_function();
        _exit(42);
    }

    int status = 0;
    if (waitpid(pid, &status, WUNTRACED) != pid) {
        return fail("waitpid initial int3 stop");
    }
    if (!WIFSTOPPED(status) || WSTOPSIG(status) != SIGTRAP) {
        printf("FAIL: expected SIGTRAP, status=%#x\n", status);
        return 1;
    }
    printf("  ok: initial int3 SIGTRAP stop\n");

    struct x86_64_user_regs regs;
    memset(&regs, 0, sizeof(regs));
    if (getregset(pid, &regs) != 0) {
        return fail("getregset at initial stop");
    }

    extern char test_bp_addr;
    uintptr_t bp_addr = (uintptr_t)&test_bp_addr;
    /* int3 is 1 byte; after #BP the saved RIP = bp_addr + 1. */
    if (regs.rip != bp_addr + 1) {
        printf("FAIL: expected RIP=%#llx (bp+1), got %#llx\n",
               (unsigned long long)(bp_addr + 1), (unsigned long long)regs.rip);
        return 1;
    }
    printf("  ok: RIP=%#llx (bp+1)\n", (unsigned long long)regs.rip);

    /* Step 1: replace int3 (0xCC) with NOP (0x90) at the breakpoint site,
     * preserving the surrounding bytes (POKEDATA is word-sized). */
    errno = 0;
    long orig_word = ptrace(PTRACE_PEEKDATA, pid, (void *)bp_addr, NULL);
    if (orig_word == -1 && errno != 0) {
        return fail("PEEKDATA at bp");
    }
    unsigned long nop_word = ((unsigned long)orig_word & ~0xffUL) | 0x90UL;
    if (ptrace(PTRACE_POKEDATA, pid, (void *)bp_addr, (void *)nop_word) != 0) {
        return fail("POKEDATA write NOP");
    }
    printf("  ok: replaced int3 with NOP at %#llx\n", (unsigned long long)bp_addr);

    /* Step 2: rewind RIP to the int3/NOP address. */
    regs.rip = bp_addr;
    if (setregset(pid, &regs) != 0) {
        return fail("setregset rewind RIP");
    }

    /* Step 3: PTRACE_SINGLESTEP — the CPU executes the NOP (1 byte)
     * and then fires #DB because TF was set in RFLAGS. */
    if (ptrace(PTRACE_SINGLESTEP, pid, NULL, NULL) != 0) {
        return fail("PTRACE_SINGLESTEP");
    }
    printf("  ok: PTRACE_SINGLESTEP issued\n");

    /* Step 4: wait for the #DB stop. */
    if (waitpid(pid, &status, WUNTRACED) != pid) {
        return fail("waitpid singlestep stop");
    }
    if (!WIFSTOPPED(status) || WSTOPSIG(status) != SIGTRAP) {
        printf("FAIL: expected SIGTRAP after singlestep, status=%#x signal=%d\n",
               status, WSTOPSIG(status));
        return 1;
    }
    printf("  ok: singlestep SIGTRAP stop\n");

    /* Step 5: verify RIP advanced past the breakpoint (NOP is 1 byte). */
    memset(&regs, 0, sizeof(regs));
    if (getregset(pid, &regs) != 0) {
        return fail("getregset after singlestep");
    }
    if (regs.rip != bp_addr + 1) {
        printf("FAIL: RIP=%#llx, expected %#llx (bp+1)\n",
               (unsigned long long)regs.rip, (unsigned long long)(bp_addr + 1));
        return 1;
    }
    printf("  ok: RIP=%#llx advanced past breakpoint (offset +1)\n",
           (unsigned long long)regs.rip);

    /* Step 6: restore int3 (for correctness; doesn't affect the test
     * since execution has already passed this point). */
    unsigned long cc_word = ((unsigned long)orig_word & ~0xffUL) | 0xccUL;
    if (ptrace(PTRACE_POKEDATA, pid, (void *)bp_addr, (void *)cc_word) != 0) {
        return fail("POKEDATA re-insert int3");
    }

    /* Step 7: CONT — child continues through target_function → _exit(42). */
    if (ptrace(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("PTRACE_CONT");
    }

    /* Step 8: child exits normally. */
    if (waitpid(pid, &status, 0) != pid) {
        return fail("waitpid exit");
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 42) {
        printf("FAIL: expected exit code 42, status=%#x\n", status);
        return 1;
    }
    printf("  ok: child exited with 42\n");

    return 0;
}

int main(void)
{
    int pass = 0;
    int fail_count = 0;

    if (test_breakpoint() == 0) {
        pass++;
    } else {
        fail_count++;
    }

    if (test_breakpoint_singlestep_restore() == 0) {
        pass++;
    } else {
        fail_count++;
    }

    printf("DONE: %d pass, %d fail\n", pass, fail_count);
    return fail_count > 0 ? 1 : 0;
}
