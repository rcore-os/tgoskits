#define _GNU_SOURCE

#include <stdio.h>

#if defined(__x86_64__)

#include <cpuid.h>
#include <signal.h>
#include <stdint.h>
#include <string.h>
#include <sys/syscall.h>
#include <ucontext.h>
#include <unistd.h>

#define FP_XSTATE_MAGIC1 0x46505853u
#define FP_XSTATE_MAGIC2 0x46505845u
#define FXSAVE_SW_RESERVED_OFFSET 464u
#define XSAVE_HEADER_OFFSET 512u
#define XSAVE_YMM_HI128_OFFSET 576u
#define YMM15_HI128_OFFSET (XSAVE_YMM_HI128_OFFSET + 15u * 16u)
#define XFEATURE_MASK_AVX (1ull << 2)

static volatile sig_atomic_t handler_ran;
static volatile sig_atomic_t frame_error;

static const uint64_t interrupted_pattern[4] __attribute__((aligned(32))) = {
    0x1021324354657687ull,
    0x98a9bacbdcedfe0full,
    0x1122334455667788ull,
    0x99aabbccddeeff00ull,
};

static const uint64_t handler_upper_pattern[2] __attribute__((aligned(16))) = {
    0xcafebabedeadbeefull,
    0x0123456789abcdefull,
};

__attribute__((naked, noinline)) static void clear_live_avx_state(void)
{
    __asm__ volatile("vzeroall\n\tret\n\t");
}

__attribute__((naked, noinline)) static void
signal_and_capture_ymm15(pid_t pid, pid_t tid, uint64_t result[4],
                         const uint64_t pattern[4])
{
    __asm__ volatile(
        "mov %rdx, %r8\n\t"
        "mov %rcx, %r9\n\t"
        "vmovdqu (%r9), %ymm15\n\t"
        "mov $10, %edx\n\t"
        "mov $234, %eax\n\t"
        "syscall\n\t"
        "vmovdqu %ymm15, (%r8)\n\t"
        "ret\n\t");
}

static uint32_t load_u32(const unsigned char *bytes)
{
    uint32_t value;
    memcpy(&value, bytes, sizeof(value));
    return value;
}

static uint64_t load_u64(const unsigned char *bytes)
{
    uint64_t value;
    memcpy(&value, bytes, sizeof(value));
    return value;
}

static void store_u64(unsigned char *bytes, uint64_t value)
{
    memcpy(bytes, &value, sizeof(value));
}

static void signal_handler(int signo, siginfo_t *info, void *opaque_context)
{
    (void)signo;
    (void)info;
    handler_ran = 1;

    ucontext_t *context = opaque_context;
    unsigned char *fpstate = (unsigned char *)context->uc_mcontext.fpregs;
    if (fpstate == NULL) {
        frame_error = 1;
        clear_live_avx_state();
        return;
    }

    const unsigned char *sw_reserved = fpstate + FXSAVE_SW_RESERVED_OFFSET;
    uint32_t magic1 = load_u32(sw_reserved);
    uint32_t extended_size = load_u32(sw_reserved + 4);
    uint64_t xfeatures = load_u64(sw_reserved + 8);
    uint32_t xstate_size = load_u32(sw_reserved + 16);
    if (magic1 != FP_XSTATE_MAGIC1 || extended_size != xstate_size + 4 ||
        xstate_size < YMM15_HI128_OFFSET + 16 ||
        (xfeatures & XFEATURE_MASK_AVX) == 0 ||
        load_u32(fpstate + xstate_size) != FP_XSTATE_MAGIC2) {
        frame_error = 2;
        clear_live_avx_state();
        return;
    }

    uint64_t xstate_bv = load_u64(fpstate + XSAVE_HEADER_OFFSET);
    uint64_t xcomp_bv = load_u64(fpstate + XSAVE_HEADER_OFFSET + 8);
    if ((xstate_bv & XFEATURE_MASK_AVX) == 0 || xcomp_bv != 0) {
        frame_error = 3;
        clear_live_avx_state();
        return;
    }

    memcpy(fpstate + YMM15_HI128_OFFSET, handler_upper_pattern,
           sizeof(handler_upper_pattern));
    store_u64(fpstate + XSAVE_HEADER_OFFSET, xstate_bv | XFEATURE_MASK_AVX);

    // The post-handler value must come from rt_sigreturn's frame restore, not
    // from accidentally retaining the handler's physical AVX registers.
    clear_live_avx_state();
}

static int avx_available(void)
{
    unsigned int eax, ebx, ecx, edx;
    if (!__get_cpuid(1, &eax, &ebx, &ecx, &edx) ||
        (ecx & bit_XSAVE) == 0 || (ecx & bit_OSXSAVE) == 0 ||
        (ecx & bit_AVX) == 0) {
        return 0;
    }
    uint32_t xcr0_low;
    uint32_t xcr0_high;
    __asm__ volatile("xgetbv" : "=a"(xcr0_low), "=d"(xcr0_high) : "c"(0));
    return xcr0_high == 0 && (xcr0_low & 0x7) == 0x7;
}

int main(void)
{
    if (!avx_available()) {
        puts("SKIP: x86 AVX signal xstate is unavailable");
        return 0;
    }

    struct sigaction action = {0};
    action.sa_sigaction = signal_handler;
    action.sa_flags = SA_SIGINFO;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGUSR1, &action, NULL) != 0) {
        perror("sigaction");
        return 1;
    }

    uint64_t result[4] __attribute__((aligned(32))) = {0};
    pid_t pid = getpid();
    pid_t tid = (pid_t)syscall(SYS_gettid);
    signal_and_capture_ymm15(pid, tid, result, interrupted_pattern);

    if (!handler_ran || frame_error != 0) {
        fprintf(stderr, "FAIL: signal xstate frame error=%d handler=%d\n",
                frame_error, handler_ran);
        return 1;
    }
    if (memcmp(result, interrupted_pattern, 16) != 0 ||
        memcmp(result + 2, handler_upper_pattern,
               sizeof(handler_upper_pattern)) != 0) {
        fputs("FAIL: rt_sigreturn did not restore the handler-modified YMM15 frame\n",
              stderr);
        return 1;
    }

    puts("PASS: x86 rt_sigreturn restores handler-modified AVX xstate");
    return 0;
}

#else

int main(void)
{
    puts("SKIP: x86 AVX sigreturn test is x86_64-only");
    return 0;
}

#endif
