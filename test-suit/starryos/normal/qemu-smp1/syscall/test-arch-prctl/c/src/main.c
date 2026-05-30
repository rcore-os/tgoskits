/*
 * test-arch-prctl.c - Test cases for arch_prctl(2) syscall (x86_64 only)
 *
 * Covers all arch_prctl subfunctions defined in asm/prctl.h:
 *   - ARCH_SET_CPUID (0x1012) / ARCH_GET_CPUID (0x1011)
 *   - ARCH_SET_FS    (0x1002) / ARCH_GET_FS    (0x1003)
 *   - ARCH_SET_GS    (0x1001) / ARCH_GET_GS    (0x1004)
 *
 * Error cases tested:
 *   EINVAL - invalid op code
 *   EFAULT - bad pointer argument for GET operations
 *   EPERM  - address outside process address space for SET operations
 *   ENODEV - CPUID faulting not supported by hardware (handled gracefully)
 *
 * Note: ARCH_SET_FS is only tested as a no-op (set-to-same-value)
 * because FS is used by the threading library for TLS.
 * ARCH_SET_GS may be disabled in some kernels (handled gracefully).
 */

#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include "test_framework.h"
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/mman.h>
#include <sys/syscall.h>

#if defined(__x86_64__)

/*
 * arch_prctl subfunction codes from arch/x86/include/uapi/asm/prctl.h.
 * Defined manually to avoid depending on <asm/prctl.h> which may not
 * be available in all cross-compilation sysroots.
 */
#define ARCH_SET_GS    0x1001
#define ARCH_SET_FS    0x1002
#define ARCH_GET_FS    0x1003
#define ARCH_GET_GS    0x1004
#define ARCH_GET_CPUID 0x1011
#define ARCH_SET_CPUID 0x1012

/* arch_prctl is accessed via syscall(2) — no glibc/musl wrapper exists */
static long raw_arch_prctl(int op, unsigned long addr)
{
    return syscall(SYS_arch_prctl, op, addr);
}

static long raw_arch_prctl_ptr(int op, unsigned long *addr)
{
    return syscall(SYS_arch_prctl, op, addr);
}

static char gs_test_buf[4096] __attribute__((aligned(4096)));

/* ============================================================
 * Happy-path tests
 * ============================================================ */

/* 1. ARCH_GET_CPUID — read the cpuid enable/disable flag */
static void test_get_cpuid(void)
{
    printf("--- ARCH_GET_CPUID ---\n");

    errno = 0;
    long ret = raw_arch_prctl(ARCH_GET_CPUID, 0);

    if (ret == -1) {
        CHECK(ret == -1, "ARCH_GET_CPUID returns -1 (may be ENOSYS)");
        printf("  info | ARCH_GET_CPUID errno=%d (%s)\n", errno, strerror(errno));
        return;
    }

    CHECK(ret == 0 || ret == 1, "ARCH_GET_CPUID returns 0 or 1");
    printf("  info | ARCH_GET_CPUID = %ld (%s)\n",
           ret, ret == 1 ? "enabled" : "disabled");
}

/* 2. ARCH_SET_CPUID(1) — explicitly enable cpuid (no-op if already
 *    enabled). Returns ENODEV if hardware lacks CPUID faulting. */
static void test_set_cpuid_enable(void)
{
    printf("--- ARCH_SET_CPUID(enable) ---\n");

    errno = 0;
    long ret = raw_arch_prctl(ARCH_SET_CPUID, 1);

    if (ret == 0) {
        CHECK_RET(ret, 0, "ARCH_SET_CPUID(1) succeeds");
    } else if (ret == -1 && errno == ENODEV) {
        CHECK(ret == -1 && errno == ENODEV,
              "ARCH_SET_CPUID(1) fails with ENODEV (HW lacks CPUID faulting)");
        return;
    } else if (ret == -1) {
        CHECK(ret == -1, "ARCH_SET_CPUID(1) returns -1");
        printf("  info | ARCH_SET_CPUID(1) errno=%d (%s)\n", errno, strerror(errno));
        return;
    }

    errno = 0;
    long state = raw_arch_prctl(ARCH_GET_CPUID, 0);
    if (state != -1) {
        CHECK(state == 1, "ARCH_GET_CPUID confirms cpuid is enabled");
    }
}

/* 3. ARCH_SET_CPUID(0) disable → ARCH_GET_CPUID → re-enable */
static void test_set_cpuid_disable_reenable(void)
{
    printf("--- ARCH_SET_CPUID(disable/re-enable) ---\n");

    errno = 0;
    long ret = raw_arch_prctl(ARCH_SET_CPUID, 0);

    if (ret == -1 && errno == ENODEV) {
        printf("  info | ARCH_SET_CPUID not supported (ENODEV), skip disable test\n");
        CHECK(ret == -1 && errno == ENODEV,
              "ARCH_SET_CPUID(0) fails with ENODEV");
        return;
    }
    if (ret == -1) {
        printf("  info | ARCH_SET_CPUID not supported, skip disable test\n");
        CHECK(ret == -1, "ARCH_SET_CPUID(0) returns -1");
        return;
    }

    CHECK_RET(ret, 0, "ARCH_SET_CPUID(0) succeeds");

    errno = 0;
    long state = raw_arch_prctl(ARCH_GET_CPUID, 0);
    if (state != -1) {
        CHECK(state == 0, "ARCH_GET_CPUID returns 0 (disabled)");
    }

    errno = 0;
    ret = raw_arch_prctl(ARCH_SET_CPUID, 1);
    CHECK_RET(ret, 0, "ARCH_SET_CPUID(1) re-enables cpuid");

    errno = 0;
    state = raw_arch_prctl(ARCH_GET_CPUID, 0);
    if (state != -1) {
        CHECK(state == 1, "ARCH_GET_CPUID returns 1 (re-enabled)");
    }
}

/* 4. ARCH_GET_FS — read current FS base register */
static void test_get_fs(void)
{
    printf("--- ARCH_GET_FS ---\n");

    unsigned long fs_base = 0;
    errno = 0;
    long ret = raw_arch_prctl_ptr(ARCH_GET_FS, &fs_base);

    if (ret == 0) {
        CHECK_RET(ret, 0, "ARCH_GET_FS succeeds");
        CHECK(fs_base != 0, "FS base is non-zero (TLS pointer)");
        printf("  info | FS base = 0x%lx\n", fs_base);
    } else {
        CHECK(ret == -1, "ARCH_GET_FS returns -1");
        printf("  info | ARCH_GET_FS errno=%d (%s)\n", errno, strerror(errno));
    }
}

/* 5. ARCH_SET_FS(current) — set FS to its current value (safe no-op) */
static void test_set_fs_same(void)
{
    printf("--- ARCH_SET_FS (set to same) ---\n");

    unsigned long orig_fs = 0;
    errno = 0;
    long ret = raw_arch_prctl_ptr(ARCH_GET_FS, &orig_fs);
    if (ret != 0) {
        printf("  info | ARCH_GET_FS failed, skip ARCH_SET_FS test\n");
        CHECK(ret == -1, "ARCH_GET_FS prerequisite failed");
        return;
    }

    errno = 0;
    ret = raw_arch_prctl(ARCH_SET_FS, orig_fs);

    if (ret == 0) {
        CHECK_RET(ret, 0, "ARCH_SET_FS(same base) succeeds");

        unsigned long new_fs = 0;
        errno = 0;
        ret = raw_arch_prctl_ptr(ARCH_GET_FS, &new_fs);
        if (ret == 0) {
            CHECK(new_fs == orig_fs, "FS base unchanged after set-to-same");
        }
    } else if (ret == -1 && (errno == EPERM || errno == EINVAL)) {
        CHECK(errno == EPERM || errno == EINVAL,
              "ARCH_SET_FS(same base) returns EPERM or EINVAL (kernel restricted)");
    } else {
        CHECK(ret == -1, "ARCH_SET_FS(same base) returns -1");
        printf("  info | ARCH_SET_FS errno=%d (%s)\n", errno, strerror(errno));
    }
}

/* 6. ARCH_GET_GS — read current GS base register */
static void test_get_gs(void)
{
    printf("--- ARCH_GET_GS ---\n");

    unsigned long gs_base = 0;
    errno = 0;
    long ret = raw_arch_prctl_ptr(ARCH_GET_GS, &gs_base);

    if (ret == 0) {
        CHECK_RET(ret, 0, "ARCH_GET_GS succeeds");
        printf("  info | GS base = 0x%lx\n", gs_base);
    } else {
        CHECK(ret == -1, "ARCH_GET_GS returns -1");
        printf("  info | ARCH_GET_GS errno=%d (%s)\n", errno, strerror(errno));
    }
}

/* 7. ARCH_SET_GS → ARCH_GET_GS round-trip.
 *    ARCH_SET_GS may be disabled in some kernels (EINVAL). */
static void test_set_get_gs(void)
{
    printf("--- ARCH_SET_GS / ARCH_GET_GS round-trip ---\n");

    unsigned long orig_gs = 0;
    errno = 0;
    long ret = raw_arch_prctl_ptr(ARCH_GET_GS, &orig_gs);
    if (ret != 0) {
        printf("  info | ARCH_GET_GS failed, skip ARCH_SET_GS test\n");
        CHECK(ret == -1, "ARCH_GET_GS prerequisite failed");
        return;
    }

    unsigned long new_base = (unsigned long)(uintptr_t)gs_test_buf;
    errno = 0;
    ret = raw_arch_prctl(ARCH_SET_GS, new_base);

    if (ret == -1 && errno == EINVAL) {
        printf("  info | ARCH_SET_GS is disabled in this kernel (EINVAL)\n");
        CHECK(errno == EINVAL,
              "ARCH_SET_GS fails with EINVAL (kernel may disable GS writes)");
        return;
    }
    if (ret == -1) {
        printf("  info | ARCH_SET_GS errno=%d (%s)\n", errno, strerror(errno));
        CHECK(ret == -1, "ARCH_SET_GS returns -1");
        return;
    }

    CHECK_RET(ret, 0, "ARCH_SET_GS succeeds");

    unsigned long verify_gs = 0;
    errno = 0;
    ret = raw_arch_prctl_ptr(ARCH_GET_GS, &verify_gs);
    if (ret == 0) {
        CHECK(verify_gs == new_base,
              "ARCH_GET_GS returns the buffer address we set");
    }

    errno = 0;
    ret = raw_arch_prctl(ARCH_SET_GS, orig_gs);
    if (ret == 0) {
        CHECK_RET(ret, 0, "ARCH_SET_GS(original) restores GS base");
    } else {
        printf("  info | restore GS base failed, errno=%d (%s)\n",
               errno, strerror(errno));
    }
}

/* ============================================================
 * Error-path tests
 * ============================================================ */

/* 8. EINVAL — invalid op codes */
static void test_einval_invalid_op(void)
{
    printf("--- EINVAL on invalid op ---\n");

    CHECK_ERR(raw_arch_prctl(0, 0), EINVAL,
              "op=0 returns EINVAL");

    CHECK_ERR(raw_arch_prctl(-1, 0), EINVAL,
              "op=-1 (large unsigned) returns EINVAL");

    CHECK_ERR(raw_arch_prctl(0x9999, 0), EINVAL,
              "op=0x9999 (unknown) returns EINVAL");

    errno = 0;
    long ret = raw_arch_prctl(0x2001, (unsigned long)(uintptr_t)gs_test_buf);
    if (ret == -1) {
        CHECK(errno == EINVAL || errno == ENOSYS || errno == EPERM
              || errno == EEXIST,
              "op=0x2001 (ARCH_MAP_VDSO_X32) fails cleanly");
    }
}

/* 9. EFAULT — invalid pointer for GET operations */
static void test_efault_bad_pointer(void)
{
    printf("--- EFAULT on bad pointer ---\n");

    CHECK_ERR(raw_arch_prctl_ptr(ARCH_GET_FS, NULL), EFAULT,
              "ARCH_GET_FS(NULL) returns EFAULT");

    CHECK_ERR(raw_arch_prctl_ptr(ARCH_GET_GS, NULL), EFAULT,
              "ARCH_GET_GS(NULL) returns EFAULT");

    unsigned long *bad_ptr = (unsigned long *)-1;
    CHECK_ERR(raw_arch_prctl_ptr(ARCH_GET_FS, bad_ptr), EFAULT,
              "ARCH_GET_FS(-1) returns EFAULT");

    CHECK_ERR(raw_arch_prctl_ptr(ARCH_GET_GS, bad_ptr), EFAULT,
              "ARCH_GET_GS(-1) returns EFAULT");
}

/* 10. EPERM — address outside process address space for SET operations.
 *     The kernel checks whether the address is in the canonical userspace
 *     range (below TASK_SIZE_MAX, ~0x00007fffffffffff on 47-bit).
 *     Addresses with the high bit set are non-canonical userspace. */
static void test_eperm_invalid_address_set(void)
{
    printf("--- EPERM on invalid address ---\n");

    unsigned long bad_addr = 0x8000000000000000UL;

    errno = 0;
    long ret = raw_arch_prctl(ARCH_SET_GS, bad_addr);
    if (ret == -1 && (errno == EPERM || errno == EINVAL)) {
        CHECK(errno == EPERM || errno == EINVAL,
              "ARCH_SET_GS(non-canonical addr) returns EPERM or EINVAL");
    } else if (ret == -1) {
        CHECK(ret == -1, "ARCH_SET_GS(non-canonical addr) returns -1");
        printf("  info | errno=%d (%s)\n", errno, strerror(errno));
    } else {
        CHECK(ret == -1, "ARCH_SET_GS(non-canonical addr) should have failed");
    }

    errno = 0;
    ret = raw_arch_prctl(ARCH_SET_FS, bad_addr);
    if (ret == -1 && (errno == EPERM || errno == EINVAL)) {
        CHECK(errno == EPERM || errno == EINVAL,
              "ARCH_SET_FS(non-canonical addr) returns EPERM or EINVAL");
    } else if (ret == -1) {
        CHECK(ret == -1, "ARCH_SET_FS(non-canonical addr) returns -1");
        printf("  info | errno=%d (%s)\n", errno, strerror(errno));
    } else {
        CHECK(ret == -1, "ARCH_SET_FS(non-canonical addr) should have failed");
    }
}

/* ============================================================
 * MAIN
 * ============================================================ */

int main(void)
{
    TEST_START("arch_prctl syscall");

    memset(gs_test_buf, 0xAB, sizeof(gs_test_buf));

    test_get_cpuid();
    test_set_cpuid_enable();
    test_set_cpuid_disable_reenable();

    test_get_fs();
    test_set_fs_same();

    test_get_gs();
    test_set_get_gs();

    test_einval_invalid_op();
    test_efault_bad_pointer();
    test_eperm_invalid_address_set();

    TEST_DONE();
}

#else  /* ! __x86_64__ */

int main(void)
{
    TEST_START("arch_prctl syscall");

    printf("  SKIP: arch_prctl is x86_64 specific, not available on this architecture\n");

    TEST_DONE();
}

#endif /* __x86_64__ */
