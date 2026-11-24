/*
 * test_mrs_idreg.c — EL0 MRS of the AArch64 ID_AA64* feature registers.
 *
 * On AArch64, reading an ID_AA64*_EL1 register from EL0 is UNDEFINED and traps;
 * the arm64 Go/.NET runtimes do exactly this for CPU-feature detection, so the
 * kernel must emulate it (like Linux's emulate_mrs) instead of delivering
 * SIGILL. The emulation must also hand back a *sanitized* user value: features
 * that need kernel context-save/enable (SVE, PAuth, ...) must read as 0, or a
 * program would try to use them and crash.
 *
 * This test only runs on aarch64; other arches skip (the registers do not
 * exist). It is built with the musl cross sysroot, so the registers are named
 * via raw `mrs` in inline asm.
 */

#include "test_framework.h"
#include <stdint.h>

int main(void)
{
    TEST_START("aarch64 MRS ID_AA64* emulation + sanitization");

#if defined(__aarch64__)
    /* If emulation is missing this MRS delivers SIGILL and the process dies
     * before printing anything; reaching the assertion means it was emulated. */
    uint64_t isar0;
    __asm__ volatile("mrs %0, ID_AA64ISAR0_EL1" : "=r"(isar0));
    CHECK(1, "EL0 MRS ID_AA64ISAR0_EL1 emulated (no SIGILL)");

    /* ID_AA64PFR0_EL1: SVE field [35:32] must be sanitized to 0 (the kernel has
     * no SVE context-save), while baseline FP[19:16]/AdvSIMD[23:20] are kept. */
    uint64_t pfr0;
    __asm__ volatile("mrs %0, ID_AA64PFR0_EL1" : "=r"(pfr0));
    CHECK(((pfr0 >> 32) & 0xF) == 0, "ID_AA64PFR0_EL1 SVE field sanitized to 0");

    /* ID_AA64ISAR1_EL1: PAuth fields APA[7:4]/API[11:8]/GPA[27:24]/GPI[31:28]
     * must be sanitized to 0 (no kernel key management). */
    uint64_t isar1;
    __asm__ volatile("mrs %0, ID_AA64ISAR1_EL1" : "=r"(isar1));
    CHECK(((isar1 >> 4) & 0xFF) == 0,
          "ID_AA64ISAR1_EL1 PAuth APA/API fields sanitized to 0");
    CHECK(((isar1 >> 24) & 0xFF) == 0,
          "ID_AA64ISAR1_EL1 PAuth GPA/GPI fields sanitized to 0");

    /* A register outside the exposed set (e.g. ID_AA64DFR0_EL1) is reported as
     * not-implemented (RAZ) rather than SIGILL or a raw leak. */
    uint64_t dfr0;
    __asm__ volatile("mrs %0, ID_AA64DFR0_EL1" : "=r"(dfr0));
    CHECK(dfr0 == 0, "ID_AA64DFR0_EL1 reads as 0 (RAZ, not a raw EL1 leak)");
#else
    CHECK(1, "non-aarch64: ID_AA64* MRS emulation test skipped");
#endif

    TEST_DONE();
}
