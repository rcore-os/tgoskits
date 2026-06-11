/*
 * test_avx_vex.c — userspace VEX-encoded AVX must not #UD when the CPU has AVX.
 *
 * On x86, executing a VEX-encoded AVX instruction at CPL3 requires the kernel to
 * have set CR4.OSXSAVE and enabled XCR0.{X87,SSE,AVX}; otherwise the instruction
 * raises #UD (-> SIGILL). someboot now does this CPUID-guarded per-CPU at boot
 * (trap.rs::enable_xsave_features, #1112). This test is CPUID-gated: it runs the
 * AVX path only when the CPU advertises XSAVE + AVX, so it exercises the fix on
 * an AVX-capable QEMU (the system group's qemu-x86_64.toml uses -cpu
 * Haswell,+avx) and skips cleanly on a CPU without XSAVE. x86-only; other arches
 * skip.
 */

#include "test_framework.h"

#if defined(__x86_64__)
#include <cpuid.h>
#endif

int main(void)
{
    TEST_START("userspace VEX AVX does not #UD (#1112)");

#if defined(__x86_64__)
    unsigned int eax, ebx, ecx, edx;
    if (!__get_cpuid(1, &eax, &ebx, &ecx, &edx)) {
        CHECK(1, "CPUID leaf 1 unavailable: AVX test skipped");
        TEST_DONE();
    }
    int has_xsave = (ecx >> 26) & 1;
    int has_avx = (ecx >> 28) & 1;
    if (!has_xsave || !has_avx) {
        /* No XSAVE/AVX (e.g. default qemu64): the fix correctly leaves XCR0 off
         * and AVX would legitimately #UD, so there is nothing to assert here. */
        CHECK(1, "CPU has no XSAVE/AVX: AVX test skipped");
    } else {
        /* OSXSAVE (ECX bit 27) reflects CR4.OSXSAVE, which the kernel must have
         * enabled before XSETBV; it is the visible effect of the fix. */
        CHECK((ecx >> 27) & 1,
              "CR4.OSXSAVE enabled by kernel (CPUID.01H OSXSAVE bit set)");

        /* A VEX-encoded AVX instruction at CPL3: must execute, not #UD. Reaching
         * the next line means no SIGILL was delivered. */
        __asm__ volatile("vxorps %%ymm0, %%ymm0, %%ymm0" ::: "ymm0");
        CHECK(1, "VEX-encoded AVX (vxorps ymm0) executed at CPL3 without #UD");
    }
#else
    CHECK(1, "non-x86_64: AVX VEX test skipped");
#endif

    TEST_DONE();
}
