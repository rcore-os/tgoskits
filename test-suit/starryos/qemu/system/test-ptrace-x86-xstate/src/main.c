#define _GNU_SOURCE

#include <stdio.h>

#if defined(__x86_64__)

#include <cpuid.h>
#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/ptrace.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef NT_PRFPREG
#define NT_PRFPREG 2
#endif
#ifndef NT_X86_XSTATE
#define NT_X86_XSTATE 0x202
#endif

#define FXSAVE_SIZE 512u
#define FXSAVE_XMM0_OFFSET 160u
#define XSAVE_HEADER_OFFSET 512u
#define XSAVE_YMM_HI128_OFFSET 576u
#define XSAVE_BUFFER_SIZE 4096u
#define XFEATURE_MASK_SSE_AVX ((1ull << 1) | (1ull << 2))

struct shared_result {
    uint64_t ymm0[4];
};

struct xsave_buffer {
    unsigned char bytes[XSAVE_BUFFER_SIZE];
} __attribute__((aligned(64)));

static const uint64_t tracee_pattern[4] __attribute__((aligned(32))) = {
    0x1021324354657687ull,
    0x98a9bacbdcedfe0full,
    0x1122334455667788ull,
    0x99aabbccddeeff00ull,
};

static const uint64_t tracer_pattern[4] __attribute__((aligned(32))) = {
    0x0f1e2d3c4b5a6978ull,
    0x8796a5b4c3d2e1f0ull,
    0xcafebabedeadbeefull,
    0x0123456789abcdefull,
};

__attribute__((naked, noinline, noreturn)) static void
load_ymm0_trap_store_and_exit(const uint64_t *pattern, struct shared_result *result)
{
    __asm__ volatile(
        "vmovdqu (%rdi), %ymm0\n\t"
        "int3\n\t"
        "vmovdqu %ymm0, (%rsi)\n\t"
        "mov $60, %eax\n\t"
        "xor %edi, %edi\n\t"
        "syscall\n\t"
        "ud2\n\t");
}

static int expect_stop(pid_t pid, int signal)
{
    int status = 0;
    if (waitpid(pid, &status, 0) != pid) {
        perror("waitpid");
        return -1;
    }
    if (!WIFSTOPPED(status) || WSTOPSIG(status) != signal) {
        fprintf(stderr, "FAIL: expected stop signal %d, status=%#x\n", signal, status);
        return -1;
    }
    return 0;
}

static int xsave_user_size(size_t *size)
{
    unsigned int eax, ebx, ecx, edx;
    if (!__get_cpuid_count(0x0d, 0, &eax, &ebx, &ecx, &edx)) {
        fputs("FAIL: CPUID leaf 0x0d is unavailable\n", stderr);
        return -1;
    }
    if (ebx < XSAVE_YMM_HI128_OFFSET + 256u || ebx > XSAVE_BUFFER_SIZE) {
        fprintf(stderr, "FAIL: unexpected enabled xstate size %u\n", ebx);
        return -1;
    }
    *size = ebx;
    return 0;
}

static int check_pattern(const unsigned char *xstate, const uint64_t pattern[4],
                         const char *operation)
{
    if (memcmp(xstate + FXSAVE_XMM0_OFFSET, pattern, 16) != 0 ||
        memcmp(xstate + XSAVE_YMM_HI128_OFFSET, pattern + 2, 16) != 0) {
        fprintf(stderr,
                "FAIL: %s did not expose the tracee's complete YMM0 state\n",
                operation);
        return -1;
    }
    return 0;
}

static int expect_ptrace_errno(long result, int expected, const char *operation)
{
    if (result == -1 && errno == expected) {
        return 0;
    }
    fprintf(stderr, "FAIL: %s returned %ld errno=%d, expected errno=%d\n",
            operation, result, errno, expected);
    return -1;
}

static int run_test(void)
{
    struct shared_result *result = mmap(NULL, sizeof(*result), PROT_READ | PROT_WRITE,
                                        MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (result == MAP_FAILED) {
        perror("mmap");
        return 1;
    }

    pid_t child = fork();
    if (child < 0) {
        perror("fork");
        munmap(result, sizeof(*result));
        return 1;
    }
    if (child == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(70);
        }
        if (raise(SIGSTOP) != 0) {
            _exit(71);
        }
        load_ymm0_trap_store_and_exit(tracee_pattern, result);
    }

    int failed = 0;
    if (expect_stop(child, SIGSTOP) != 0 ||
        ptrace(PTRACE_CONT, child, NULL, NULL) != 0 ||
        expect_stop(child, SIGTRAP) != 0) {
        perror("establish ptrace trap stop");
        failed = 1;
        goto kill_child;
    }

    unsigned char fxsave[FXSAVE_SIZE] __attribute__((aligned(16))) = {0};
    if (ptrace(PTRACE_GETFPREGS, child, NULL, fxsave) != 0 ||
        memcmp(fxsave + FXSAVE_XMM0_OFFSET, tracee_pattern, 16) != 0) {
        perror("PTRACE_GETFPREGS latest stopped XMM0");
        failed = 1;
        goto kill_child;
    }

    memset(fxsave, 0, sizeof(fxsave));
    struct iovec fp_iov = {.iov_base = fxsave, .iov_len = sizeof(fxsave)};
    if (ptrace(PTRACE_GETREGSET, child, NT_PRFPREG, &fp_iov) != 0 ||
        fp_iov.iov_len != sizeof(fxsave) ||
        memcmp(fxsave + FXSAVE_XMM0_OFFSET, tracee_pattern, 16) != 0) {
        perror("PTRACE_GETREGSET NT_PRFPREG latest stopped XMM0");
        failed = 1;
        goto kill_child;
    }

    fp_iov.iov_len = sizeof(fxsave) - 1;
    errno = 0;
    if (expect_ptrace_errno(
            ptrace(PTRACE_GETREGSET, child, NT_PRFPREG, &fp_iov), EINVAL,
            "misaligned NT_PRFPREG GET") != 0) {
        failed = 1;
        goto kill_child;
    }

    unsigned char long_fxsave[FXSAVE_SIZE + 8] = {0};
    memcpy(long_fxsave, fxsave, sizeof(fxsave));
    fp_iov.iov_base = long_fxsave;
    fp_iov.iov_len = sizeof(long_fxsave);
    if (ptrace(PTRACE_SETREGSET, child, NT_PRFPREG, &fp_iov) != 0 ||
        fp_iov.iov_len != sizeof(fxsave)) {
        perror("oversized NT_PRFPREG SET truncation");
        failed = 1;
        goto kill_child;
    }

    size_t user_size = 0;
    if (xsave_user_size(&user_size) != 0) {
        failed = 1;
        goto kill_child;
    }

    struct xsave_buffer xstate = {0};
    struct iovec xstate_iov = {.iov_base = &xstate, .iov_len = sizeof(xstate)};
    errno = 0;
    if (ptrace(PTRACE_GETREGSET, child, NT_X86_XSTATE, &xstate_iov) != 0) {
        fprintf(stderr,
                "FAIL: PTRACE_GETREGSET NT_X86_XSTATE: errno=%d (%s)\n",
                errno, strerror(errno));
        failed = 1;
        goto kill_child;
    }
    if (xstate_iov.iov_len != user_size ||
        check_pattern(xstate.bytes, tracee_pattern, "NT_X86_XSTATE GET") != 0) {
        fprintf(stderr, "FAIL: NT_X86_XSTATE length=%zu expected=%zu\n",
                xstate_iov.iov_len, user_size);
        failed = 1;
        goto kill_child;
    }

    struct iovec invalid_iov = {
        .iov_base = &xstate,
        .iov_len = user_size - 1,
    };
    errno = 0;
    if (expect_ptrace_errno(
            ptrace(PTRACE_GETREGSET, child, NT_X86_XSTATE, &invalid_iov), EINVAL,
            "misaligned NT_X86_XSTATE GET") != 0) {
        failed = 1;
        goto kill_child;
    }

    invalid_iov.iov_len = user_size - 8;
    errno = 0;
    if (expect_ptrace_errno(
            ptrace(PTRACE_SETREGSET, child, NT_X86_XSTATE, &invalid_iov), EFAULT,
            "short NT_X86_XSTATE SET") != 0) {
        failed = 1;
        goto kill_child;
    }

    struct xsave_buffer invalid_xstate = xstate;
    uint64_t invalid_xcomp_bv = 1;
    memcpy(invalid_xstate.bytes + XSAVE_HEADER_OFFSET + sizeof(uint64_t),
           &invalid_xcomp_bv, sizeof(invalid_xcomp_bv));
    invalid_iov.iov_base = &invalid_xstate;
    invalid_iov.iov_len = user_size;
    errno = 0;
    if (expect_ptrace_errno(
            ptrace(PTRACE_SETREGSET, child, NT_X86_XSTATE, &invalid_iov), EINVAL,
            "compacted NT_X86_XSTATE SET") != 0) {
        failed = 1;
        goto kill_child;
    }

    invalid_xstate = xstate;
    uint32_t invalid_mxcsr = UINT32_MAX;
    memcpy(invalid_xstate.bytes + 24, &invalid_mxcsr, sizeof(invalid_mxcsr));
    invalid_iov.iov_base = &invalid_xstate;
    errno = 0;
    if (expect_ptrace_errno(
            ptrace(PTRACE_SETREGSET, child, NT_X86_XSTATE, &invalid_iov), EINVAL,
            "reserved MXCSR bits in NT_X86_XSTATE SET") != 0) {
        failed = 1;
        goto kill_child;
    }

    struct xsave_buffer user_mask_xstate = xstate;
    uint32_t user_mxcsr = 0;
    uint32_t user_mxcsr_mask = 1;
    memcpy(user_mask_xstate.bytes + 24, &user_mxcsr, sizeof(user_mxcsr));
    memcpy(user_mask_xstate.bytes + 28, &user_mxcsr_mask,
           sizeof(user_mxcsr_mask));
    invalid_iov.iov_base = &user_mask_xstate;
    if (ptrace(PTRACE_SETREGSET, child, NT_X86_XSTATE, &invalid_iov) != 0) {
        perror("valid NT_X86_XSTATE SET with user MXCSR mask payload");
        failed = 1;
        goto kill_child;
    }
    invalid_iov.iov_base = &xstate;
    if (ptrace(PTRACE_SETREGSET, child, NT_X86_XSTATE, &invalid_iov) != 0) {
        perror("NT_X86_XSTATE SET must use the CPU MXCSR feature mask");
        failed = 1;
        goto kill_child;
    }

    uint64_t *xstate_bv = (uint64_t *)(xstate.bytes + XSAVE_HEADER_OFFSET);
    *xstate_bv |= XFEATURE_MASK_SSE_AVX;
    memcpy(xstate.bytes + FXSAVE_XMM0_OFFSET, tracer_pattern, 16);
    memcpy(xstate.bytes + XSAVE_YMM_HI128_OFFSET, tracer_pattern + 2, 16);
    xstate_iov.iov_len = user_size;
    if (ptrace(PTRACE_SETREGSET, child, NT_X86_XSTATE, &xstate_iov) != 0) {
        perror("PTRACE_SETREGSET NT_X86_XSTATE");
        failed = 1;
        goto kill_child;
    }

    memset(&xstate, 0, sizeof(xstate));
    xstate_iov.iov_len = sizeof(xstate);
    if (ptrace(PTRACE_GETREGSET, child, NT_X86_XSTATE, &xstate_iov) != 0 ||
        xstate_iov.iov_len != user_size ||
        check_pattern(xstate.bytes, tracer_pattern, "NT_X86_XSTATE SET/GET") != 0) {
        perror("PTRACE_GETREGSET after NT_X86_XSTATE SET");
        failed = 1;
        goto kill_child;
    }

    if (ptrace(PTRACE_CONT, child, NULL, NULL) != 0) {
        perror("PTRACE_CONT after xstate update");
        failed = 1;
        goto kill_child;
    }

    int status = 0;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status) ||
        WEXITSTATUS(status) != 0) {
        fprintf(stderr, "FAIL: tracee exit status=%#x\n", status);
        failed = 1;
        goto done;
    }
    if (memcmp(result->ymm0, tracer_pattern, sizeof(tracer_pattern)) != 0) {
        fputs("FAIL: ptrace SET xstate was overwritten before user return\n", stderr);
        failed = 1;
        goto done;
    }

    puts("PASS: ptrace x86 FP regsets preserve full stopped state and restore SET xstate");
    goto done;

kill_child:
    kill(child, SIGKILL);
    waitpid(child, NULL, 0);
done:
    munmap(result, sizeof(*result));
    return failed;
}

int main(void)
{
    return run_test();
}

#else

int main(void)
{
    puts("SKIP: ptrace x86 xstate test is x86_64-only");
    return 0;
}

#endif
