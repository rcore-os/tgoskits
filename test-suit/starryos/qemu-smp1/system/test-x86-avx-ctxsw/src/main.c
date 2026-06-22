/*
 * test_x86_avx_ctxsw.c — AVX YMM upper-128 state must survive context switches.
 *
 * The x86_64 boot path enables XCR0.AVX, so userspace freely uses the 256-bit
 * YMM registers. The kernel therefore has to save/restore the *full* extended
 * state on every task switch. FXSAVE/FXRSTOR only cover x87 + SSE (the low 128
 * bits, XMM); they silently drop the upper 128 bits of each YMM. If the kernel
 * uses FXSAVE-only context switching while XCR0.AVX is on, the YMM upper halves
 * get corrupted whenever a thread is descheduled and rescheduled — which crashes
 * every AVX-using program (e.g. the JVM) with SIGSEGV in user code. The fix
 * switches the context-switch save/restore to XSAVE/XRSTOR with the live XCR0
 * mask (gated on CR4.OSXSAVE, FXSAVE fallback otherwise).
 *
 * This test loads a known sentinel into the UPPER 128 bits of a YMM register and
 * keeps that value LIVE across many forced context switches (sched_yield in a
 * tight loop while busy sibling threads also clobber YMM with their own
 * patterns), then verifies the upper half is intact. To keep the value live
 * across the kernel boundary it never leaves an inline-asm block: the AVX ABI
 * mandates `vzeroupper` before ordinary calls, and any libc/syscall wrapper would
 * clobber YMM, so the yields are issued as raw inline `syscall`s inside the same
 * asm region that holds the sentinel.
 *
 * On the FXSAVE-only baseline this FAILs (upper half comes back corrupted); with
 * the XSAVE fix it PASSes. CPUID-gated: runs the AVX path only when the CPU
 * advertises XSAVE + AVX (the system group's qemu-x86_64.toml uses
 * -cpu Haswell,+avx) and skips cleanly otherwise. x86-only; other arches skip.
 */

#include "test_framework.h"

#if defined(__x86_64__)

#include <cpuid.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <sys/syscall.h>

/* Number of sibling threads that continuously clobber YMM with their own
 * patterns, maximizing the chance the scheduler swaps the main thread out while
 * its sentinel is live. */
#define NOISE_THREADS 4

/* Number of forced context switches the main thread drives while holding the
 * sentinel live. Large enough to span many scheduler ticks. */
#define YIELD_ROUNDS 20000

static atomic_int stop_noise = 0;

/* Each sibling thread spins, repeatedly broadcasting a distinct 256-bit pattern
 * into *every* YMM register (ymm0..ymm15) and yielding, so that whenever it runs
 * it overwrites the upper 128 bits of the same register the main thread holds its
 * sentinel in. If the kernel restores the main thread's full YMM state correctly,
 * this noise is harmless; if it only restores the low 128 bits (FXSAVE/FXRSTOR),
 * the main thread will read back this noise pattern in its sentinel's upper half.
 * Dirtying all 16 registers is what makes the FXSAVE-only regression observable
 * regardless of which YMM register the sentinel lives in. */
static void *noise_thread(void *arg)
{
    uint64_t seed = (uint64_t)(uintptr_t)arg * 0x9E3779B97F4A7C15ULL;
    /* A 32-byte pattern; its upper 128 bits differ from the main sentinel. */
    uint64_t pat[4] = {seed ^ 0x1111111111111111ULL, seed ^ 0x2222222222222222ULL,
                       seed ^ 0x5555555555555555ULL, seed ^ 0x6666666666666666ULL};
    while (!atomic_load_explicit(&stop_noise, memory_order_relaxed)) {
        __asm__ volatile(
            "vmovdqu (%0), %%ymm0\n\t"
            "vmovdqa %%ymm0, %%ymm1\n\t"
            "vmovdqa %%ymm0, %%ymm2\n\t"
            "vmovdqa %%ymm0, %%ymm3\n\t"
            "vmovdqa %%ymm0, %%ymm4\n\t"
            "vmovdqa %%ymm0, %%ymm5\n\t"
            "vmovdqa %%ymm0, %%ymm6\n\t"
            "vmovdqa %%ymm0, %%ymm7\n\t"
            "vmovdqa %%ymm0, %%ymm8\n\t"
            "vmovdqa %%ymm0, %%ymm9\n\t"
            "vmovdqa %%ymm0, %%ymm10\n\t"
            "vmovdqa %%ymm0, %%ymm11\n\t"
            "vmovdqa %%ymm0, %%ymm12\n\t"
            "vmovdqa %%ymm0, %%ymm13\n\t"
            "vmovdqa %%ymm0, %%ymm14\n\t"
            "vmovdqa %%ymm0, %%ymm15\n\t"
            :
            : "r"(pat)
            : "ymm0", "ymm1", "ymm2", "ymm3", "ymm4", "ymm5", "ymm6", "ymm7",
              "ymm8", "ymm9", "ymm10", "ymm11", "ymm12", "ymm13", "ymm14",
              "ymm15", "memory");
        __asm__ volatile("syscall"
                         :
                         : "a"(SYS_sched_yield)
                         : "rcx", "r11", "memory");
    }
    return NULL;
}

/*
 * Hold a sentinel live in ymm15's UPPER 128 bits across YIELD_ROUNDS context
 * switches, then store ymm15 out for inspection. The whole sequence — load,
 * yields, store — stays inside one asm block so the compiler never spills,
 * clobbers, or `vzeroupper`s the live YMM value, and the yields are raw inline
 * `syscall`s so no libc wrapper touches YMM.
 *
 * `out` receives the 32 bytes of ymm15 after the yields. We seed ymm15 from a
 * 32-byte source whose UPPER 128 bits carry the sentinel and whose LOWER 128
 * bits carry an unrelated value, so we can distinguish "upper half preserved"
 * from "whole register zeroed/garbage".
 */
static void hold_ymm_across_switches(const uint64_t src[4], uint64_t out[4],
                                     long rounds)
{
    register long r __asm__("rbx") =
        rounds; /* callee-saved: survives the syscalls */
    __asm__ volatile(
        "vmovdqu (%[src]), %%ymm15\n\t" /* ymm15 = sentinel (upper) + low */
        "1:\n\t"
        "mov %[ynr], %%eax\n\t" /* sched_yield, live ymm15 across it */
        "syscall\n\t"
        "dec %[rounds]\n\t"
        "jnz 1b\n\t"
        "vmovdqu %%ymm15, (%[out])\n\t" /* read back ymm15 after the switches */
        : [rounds] "+r"(r)
        : [src] "r"(src), [out] "r"(out), [ynr] "i"(SYS_sched_yield)
        : "rax", "rcx", "r11", "ymm15", "memory");
}

int main(void)
{
    TEST_START("AVX YMM upper-128 survives context switch (XSAVE)");

    unsigned int eax, ebx, ecx, edx;
    if (!__get_cpuid(1, &eax, &ebx, &ecx, &edx)) {
        CHECK(1, "CPUID leaf 1 unavailable: AVX context-switch test skipped");
        TEST_DONE();
    }
    int has_xsave = (ecx >> 26) & 1;
    int has_osxsave = (ecx >> 27) & 1;
    int has_avx = (ecx >> 28) & 1;
    if (!has_xsave || !has_avx) {
        /* No XSAVE/AVX (e.g. default qemu64): the kernel correctly leaves XCR0
         * off and userspace must not use YMM, so there is nothing to preserve. */
        CHECK(1, "CPU has no XSAVE/AVX: AVX context-switch test skipped");
        TEST_DONE();
    }

    /* OSXSAVE reflects CR4.OSXSAVE, the precondition for the kernel to use
     * XSAVE/XRSTOR on context switch at all. */
    CHECK(has_osxsave, "CR4.OSXSAVE enabled by kernel (XSAVE-based ctxsw possible)");

    /* Start sibling threads that keep dirtying YMM with their own patterns. */
    pthread_t noise[NOISE_THREADS];
    int spawned = 0;
    for (int i = 0; i < NOISE_THREADS; i++) {
        if (pthread_create(&noise[i], NULL, noise_thread,
                           (void *)(uintptr_t)(i + 1)) == 0) {
            spawned++;
        }
    }
    CHECK(spawned > 0, "spawned at least one YMM-clobbering sibling thread");

    /*
     * Sentinel: upper 128 bits are a recognizable non-zero pattern, lower 128
     * bits are a different recognizable pattern. FXSAVE preserves the lower half
     * but drops the upper half, so a buggy kernel returns the correct low 128
     * bits with a corrupted (zeroed or sibling-pattern) upper 128 bits — a clear,
     * unambiguous signature of the FXSAVE-only regression.
     */
    static const uint64_t sentinel[4] = {
        0x0F1E2D3C4B5A6978ULL, /* low[0] */
        0x8796A5B4C3D2E1F0ULL, /* low[1] */
        0xCAFEBABEDEADBEEFULL, /* high[0] — upper 128 bits */
        0x0123456789ABCDEFULL, /* high[1] — upper 128 bits */
    };
    uint64_t result[4] = {0, 0, 0, 0};

    hold_ymm_across_switches(sentinel, result, YIELD_ROUNDS);

    atomic_store_explicit(&stop_noise, 1, memory_order_relaxed);
    for (int i = 0; i < spawned; i++) {
        pthread_join(noise[i], NULL);
    }

    /* Sanity: the low 128 bits (the FXSAVE-covered part) must always survive. */
    CHECK(result[0] == sentinel[0] && result[1] == sentinel[1],
          "YMM lower 128 bits preserved across context switches");

    /* The crux: the upper 128 bits must survive too. This is what FXSAVE-only
     * context switching corrupts and what the XSAVE fix preserves. */
    int upper_ok = (result[2] == sentinel[2] && result[3] == sentinel[3]);
    if (!upper_ok) {
        printf("  upper-128 expected %016llx %016llx, got %016llx %016llx\n",
               (unsigned long long)sentinel[2], (unsigned long long)sentinel[3],
               (unsigned long long)result[2], (unsigned long long)result[3]);
    }
    CHECK(upper_ok,
          "YMM upper 128 bits preserved across context switches (XSAVE fix)");

    TEST_DONE();
}

#else /* !__x86_64__ */

int main(void)
{
    TEST_START("AVX YMM upper-128 survives context switch (XSAVE)");
    CHECK(1, "non-x86_64: AVX context-switch test skipped");
    TEST_DONE();
}

#endif
