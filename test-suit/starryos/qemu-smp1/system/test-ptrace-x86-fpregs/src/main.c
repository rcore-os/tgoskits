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

#ifndef PTRACE_TRACEME
#define PTRACE_TRACEME 0
#endif
#ifndef PTRACE_CONT
#define PTRACE_CONT 7
#endif
#ifndef PTRACE_GETFPREGS
#define PTRACE_GETFPREGS 14
#endif
#ifndef PTRACE_SETFPREGS
#define PTRACE_SETFPREGS 15
#endif
#ifndef PTRACE_GETREGSET
#define PTRACE_GETREGSET 0x4204
#endif
#ifndef NT_PRFPREG
#define NT_PRFPREG 2
#endif

// The x86_64 FXSAVE area exposed by PTRACE_GETFPREGS / NT_PRFPREG is 512 bytes.
// XMM0..XMM15 live at byte offset 160 (each register is 16 bytes), so XMM10
// (offset 320) and XMM15 (offset 400) sit *beyond* the first 256 bytes. A
// regression that only preserves 256 bytes of the FXSAVE area would therefore
// lose or corrupt those high registers while leaving XMM0 (offset 160) intact;
// this test checks both sides of that boundary on purpose.
#define FXSAVE_SIZE 512
#define XMM_OFFSET 160

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

// Known values the child loads into XMM0/XMM10/XMM15 before stopping.
static const uint64_t CHILD_XMM[3][2] = {
    {0x1111111122222222ULL, 0x3333333344444444ULL}, // xmm0
    {0x5555555566666666ULL, 0x7777777788888888ULL}, // xmm10
    {0x99999999aaaaaaaaULL, 0xbbbbbbbbccccccccULL}, // xmm15
};

// Sentinels the tracer writes back via PTRACE_SETFPREGS; the child re-reads its
// XMM registers after resuming and exits 0 only if they match.
static const uint64_t TRACER_XMM[3][2] = {
    {0xdeadbeefcafef00dULL, 0x0badc0de12345678ULL}, // xmm0
    {0xfeedfacef00dba11ULL, 0x8badf00dabad1deaULL}, // xmm10
    {0x1234567890abcdefULL, 0x0fedcba987654321ULL}, // xmm15
};

// Child side: load XMM0/10/15 from `in`, stop with SIGSTOP via a direct syscall
// (so no libc code clobbers the XMM registers between the load and the stop),
// then read the same registers back into `out` after the tracer resumes us.
static void child_fp_roundtrip(const uint64_t in[3][2], uint64_t out[3][2])
{
    pid_t pid = getpid();
    __asm__ __volatile__(
        "movups 0(%0), %%xmm0\n\t"
        "movups 16(%0), %%xmm10\n\t"
        "movups 32(%0), %%xmm15\n\t"
        "mov $62, %%rax\n\t"  // SYS_kill
        "mov %2, %%rdi\n\t"   // pid
        "mov $19, %%rsi\n\t"  // SIGSTOP
        "syscall\n\t"
        "movups %%xmm0, 0(%1)\n\t"
        "movups %%xmm10, 16(%1)\n\t"
        "movups %%xmm15, 32(%1)\n\t"
        :
        : "r"(in), "r"(out), "r"((long)pid)
        : "rax", "rcx", "rdi", "rsi", "r11", "xmm0", "xmm10", "xmm15", "memory");
}

static const uint64_t *xmm_at(const uint8_t *fxsave, int index)
{
    return (const uint64_t *)(fxsave + XMM_OFFSET + (size_t)index * 16);
}

static uint64_t *xmm_at_mut(uint8_t *fxsave, int index)
{
    return (uint64_t *)(fxsave + XMM_OFFSET + (size_t)index * 16);
}

static int check_xmm(const uint8_t *fxsave, int reg, const uint64_t expect[2],
                     const char *what)
{
    const uint64_t *xmm = xmm_at(fxsave, reg);
    if (xmm[0] != expect[0] || xmm[1] != expect[1]) {
        printf("FAIL: %s xmm%d = %016llx:%016llx, expected %016llx:%016llx\n", what,
               reg, (unsigned long long)xmm[1], (unsigned long long)xmm[0],
               (unsigned long long)expect[1], (unsigned long long)expect[0]);
        return 1;
    }
    return 0;
}

static int test_ptrace_x86_fpregs(void)
{
    printf("test: x86_64 ptrace FP (FXSAVE) registers\n");

    pid_t pid = fork();
    if (pid < 0) {
        return fail_errno("fork");
    }

    if (pid == 0) {
        if (ptrace_call(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(100);
        }
        uint64_t out[3][2] = {{0, 0}, {0, 0}, {0, 0}};
        child_fp_roundtrip(CHILD_XMM, out);
        // After resume, our XMM registers must hold the tracer's sentinels.
        for (int i = 0; i < 3; i++) {
            if (out[i][0] != TRACER_XMM[i][0] || out[i][1] != TRACER_XMM[i][1]) {
                _exit(101 + i);
            }
        }
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

    // PTRACE_GETFPREGS must return the child's full 512-byte FXSAVE area,
    // including XMM10/XMM15 which live past the first 256 bytes.
    uint8_t fxsave[FXSAVE_SIZE];
    memset(fxsave, 0, sizeof(fxsave));
    if (ptrace_call(PTRACE_GETFPREGS, pid, NULL, fxsave) != 0) {
        return fail_errno("PTRACE_GETFPREGS");
    }
    if (check_xmm(fxsave, 0, CHILD_XMM[0], "GETFPREGS") ||
        check_xmm(fxsave, 10, CHILD_XMM[1], "GETFPREGS") ||
        check_xmm(fxsave, 15, CHILD_XMM[2], "GETFPREGS")) {
        return 1;
    }
    printf("  ok: GETFPREGS returns xmm0/xmm10/xmm15 intact (incl. >256B offset)\n");

    // NT_PRFPREG (PTRACE_GETREGSET) must agree with PTRACE_GETFPREGS.
    uint8_t fxsave_set[FXSAVE_SIZE];
    memset(fxsave_set, 0, sizeof(fxsave_set));
    struct iovec iov = {.iov_base = fxsave_set, .iov_len = sizeof(fxsave_set)};
    if (ptrace_call(PTRACE_GETREGSET, pid, (void *)(intptr_t)NT_PRFPREG, &iov) != 0) {
        return fail_errno("PTRACE_GETREGSET(NT_PRFPREG)");
    }
    if (iov.iov_len != FXSAVE_SIZE) {
        return fail_msg("NT_PRFPREG returned unexpected length");
    }
    if (memcmp(fxsave, fxsave_set, FXSAVE_SIZE) != 0) {
        return fail_msg("GETFPREGS and NT_PRFPREG disagree");
    }
    printf("  ok: GETFPREGS and NT_PRFPREG agree\n");

    // Write tracer sentinels into XMM0/10/15 and push them back.
    *xmm_at_mut(fxsave, 0) = TRACER_XMM[0][0];
    *(xmm_at_mut(fxsave, 0) + 1) = TRACER_XMM[0][1];
    *xmm_at_mut(fxsave, 10) = TRACER_XMM[1][0];
    *(xmm_at_mut(fxsave, 10) + 1) = TRACER_XMM[1][1];
    *xmm_at_mut(fxsave, 15) = TRACER_XMM[2][0];
    *(xmm_at_mut(fxsave, 15) + 1) = TRACER_XMM[2][1];
    if (ptrace_call(PTRACE_SETFPREGS, pid, NULL, fxsave) != 0) {
        return fail_errno("PTRACE_SETFPREGS");
    }

    // Read it straight back to confirm SETFPREGS landed in the kernel snapshot.
    uint8_t fxsave_check[FXSAVE_SIZE];
    memset(fxsave_check, 0, sizeof(fxsave_check));
    if (ptrace_call(PTRACE_GETFPREGS, pid, NULL, fxsave_check) != 0) {
        return fail_errno("PTRACE_GETFPREGS after SET");
    }
    if (check_xmm(fxsave_check, 0, TRACER_XMM[0], "post-SET") ||
        check_xmm(fxsave_check, 10, TRACER_XMM[1], "post-SET") ||
        check_xmm(fxsave_check, 15, TRACER_XMM[2], "post-SET")) {
        return 1;
    }
    printf("  ok: SETFPREGS updated xmm0/xmm10/xmm15 in the stop snapshot\n");

    if (ptrace_call(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail_errno("PTRACE_CONT");
    }

    if (waitpid(pid, &status, 0) != pid) {
        return fail_errno("waitpid exit");
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        printf("FAIL: child did not observe tracer XMM writes, status=%#x\n", status);
        return 1;
    }
    printf("  ok: child resumed with the tracer-written XMM registers\n");
    return 0;
}

int main(void)
{
    int pass = 0;
    int fail = 0;

    if (test_ptrace_x86_fpregs() == 0) {
        pass++;
    } else {
        fail++;
    }

    printf("DONE: %d pass, %d fail\n", pass, fail);
    return fail == 0 ? 0 : 1;
}
