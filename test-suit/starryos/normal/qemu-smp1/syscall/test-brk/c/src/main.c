/*
 * test_brk.c - Test cases for brk(2) syscall
 *
 * Covers raw syscall semantics and libc wrapper behavior.
 * Uses CHECK macros (not assert) to work correctly in Release builds.
 *
 * Note: Static musl binaries may reject brk() with ENOMEM (known quirk).
 * We follow the pattern from linux-compatible-testsuit/tests/test_brk.c:
 * - Allow brk() to fail with ENOMEM for static musl
 * - But verify sbrk(0) confirms break unchanged
 */

#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
#ifndef _DEFAULT_SOURCE
#define _DEFAULT_SOURCE 1
#endif

#include "test_framework.h"
#include <stdint.h>
#include <errno.h>
#include <string.h>
#include <unistd.h>
#include <sys/syscall.h>
#include <sys/resource.h>

/* ============================================================
 * RAW SYSCALL TESTS - Verify Linux brk syscall ABI directly
 * ============================================================ */

static void test_raw_brk_query(void)
{
    unsigned long break1 = syscall(SYS_brk, 0);
    CHECK(break1 > 0, "raw brk(0) returns valid address");

    unsigned long break2 = syscall(SYS_brk, 0);
    CHECK(break2 == break1, "raw brk(0) returns consistent address");

    printf("  raw brk(0) = 0x%lx\n", break1);
}

static void test_raw_brk_expand_success(void)
{
    unsigned long current = syscall(SYS_brk, 0);
    CHECK(current > 0, "get current break for expand test");

    unsigned long new_addr = current + 4096;
    unsigned long ret = syscall(SYS_brk, new_addr);

    /* MUST return new_addr on success */
    CHECK(ret == new_addr, "raw brk(expand) returns new address");

    unsigned long after = syscall(SYS_brk, 0);
    CHECK(after == new_addr, "raw brk(0) confirms new break");

    /* Write to memory */
    memset((void *)current, 0xAA, 4096);
    CHECK(((unsigned char *)current)[0] == 0xAA, "memory write/read succeeds");

    /* Restore */
    syscall(SYS_brk, current);

    after = syscall(SYS_brk, 0);
    CHECK(after == current, "raw brk(shrink) restores original break");
}

static void test_raw_brk_shrink_success(void)
{
    unsigned long current = syscall(SYS_brk, 0);

    unsigned long expanded = current + 8192;
    unsigned long ret = syscall(SYS_brk, expanded);
    CHECK(ret == expanded, "raw brk(expand 8K) returns new address");

    ret = syscall(SYS_brk, current);
    CHECK(ret == current, "raw brk(shrink) returns original address");

    unsigned long after = syscall(SYS_brk, 0);
    CHECK(after == current, "raw brk(0) confirms shrink");
}

static void test_raw_brk_failure(void)
{
    unsigned long current = syscall(SYS_brk, 0);
    CHECK(current > 0, "get current break for failure test");

    unsigned long absurd = 1UL << 50;
    errno = 0;
    unsigned long ret = syscall(SYS_brk, absurd);

    CHECK(ret == current, "raw brk(absurd) returns current break");
    CHECK(errno == 0, "raw brk(absurd) does not set errno");

    unsigned long after = syscall(SYS_brk, 0);
    CHECK(after == current, "break unchanged after failure");
}

static void test_raw_brk_below_base(void)
{
    unsigned long current = syscall(SYS_brk, 0);
    CHECK(current > 0, "get current break for below-base test");

    errno = 0;
    unsigned long ret = syscall(SYS_brk, 0x1000);

    CHECK(ret == current, "raw brk(below base) returns current break");
    CHECK(errno == 0, "raw brk(below base) does not set errno");

    unsigned long after = syscall(SYS_brk, 0);
    CHECK(after == current, "break unchanged after below-base attempt");
}

static void test_raw_brk_roundtrip(void)
{
    unsigned long base = syscall(SYS_brk, 0);
    CHECK(base > 0, "get base for roundtrip test");

    /* Expand by multiple pages */
    unsigned long expanded1 = syscall(SYS_brk, base + 4096);
    CHECK(expanded1 == base + 4096, "raw brk(expand 4K) succeeds");

    unsigned long expanded2 = syscall(SYS_brk, base + 8192);
    CHECK(expanded2 == base + 8192, "raw brk(expand 8K) succeeds");

    /* Write to memory */
    memset((void *)base, 0xDD, 8192);
    CHECK(((unsigned char *)base)[0] == 0xDD, "memory write succeeds");
    CHECK(((unsigned char *)base)[4095] == 0xDD, "page 1 read succeeds");
    CHECK(((unsigned char *)base)[8191] == 0xDD, "page 2 read succeeds");

    /* Shrink back to base */
    unsigned long shrunk = syscall(SYS_brk, base);
    CHECK(shrunk == base, "raw brk(shrink to base) succeeds");

    unsigned long final = syscall(SYS_brk, 0);
    CHECK(final == base, "raw brk(0) confirms base restored");
}

/* ============================================================
 * LIBC WRAPPER TESTS - Test brk/sbrk via libc functions
 *
 * Note: Static musl binaries may reject brk() with ENOMEM.
 * We follow the pattern from linux-compatible-testsuit/test_brk.c:
 * - Allow brk() to fail with ENOMEM
 * - But verify sbrk(0) confirms break unchanged
 * ============================================================ */

static void test_libc_brk_current_break(void)
{
    void *cur = sbrk(0);
    CHECK(cur != (void *)-1, "sbrk(0) returns valid address");

    errno = 0;
    int ret = brk(cur);
    if (ret == 0) {
        CHECK(sbrk(0) == cur, "brk(current) succeeded, break unchanged");
    } else {
        /* Static musl may reject no-op brk() with ENOMEM */
        CHECK(ret == -1 && errno == ENOMEM, "brk(current) failed with ENOMEM (static musl quirk)");
        CHECK(sbrk(0) == cur, "break unchanged despite brk() failure");
    }
}

static void test_libc_sbrk_zero(void)
{
    void *p1 = sbrk(0);
    CHECK(p1 != (void *)-1, "sbrk(0) first call succeeds");

    void *p2 = sbrk(0);
    CHECK(p2 != (void *)-1, "sbrk(0) second call succeeds");

    CHECK(p1 == p2, "two consecutive sbrk(0) return same value");
}

static void test_libc_sbrk_allocate(void)
{
    void *old_break = sbrk(0);
    CHECK(old_break != (void *)-1, "sbrk(0) returns current break");

    errno = 0;
    void *returned = sbrk(4096);
    if (returned == (void *)-1) {
        CHECK(errno == ENOMEM, "sbrk(+4K) failed with ENOMEM");
        return;
    }

    CHECK(returned == old_break, "sbrk(+4K) returns old break (success)");

    void *new_break = sbrk(0);
    CHECK(new_break == (char *)old_break + 4096, "sbrk(0) confirms expansion");

    /* Write to allocated memory */
    memset(returned, 0x55, 4096);
    CHECK(((unsigned char *)returned)[0] == 0x55, "memory write succeeds");

    /* Restore */
    errno = 0;
    void *after_free = sbrk(-4096);
    CHECK(after_free != (void *)-1, "sbrk(-4K) succeeds");

    CHECK(sbrk(0) == old_break, "break restored to original");
}

static void test_libc_sbrk_sequential(void)
{
    void *base = sbrk(0);
    CHECK(base != (void *)-1, "sbrk(0) returns base");

    errno = 0;
    void *p1 = sbrk(4096);
    if (p1 == (void *)-1) {
        CHECK(errno == ENOMEM, "sbrk(+4K) failed with ENOMEM");
        return;
    }
    CHECK(p1 == base, "first sbrk(+4K) returns base");

    errno = 0;
    void *p2 = sbrk(4096);
    if (p2 == (void *)-1) {
        CHECK(errno == ENOMEM, "second sbrk(+4K) failed with ENOMEM");
        brk(base);
        return;
    }
    CHECK(p2 == (char *)base + 4096, "second sbrk(+4K) returns base+4K");

    errno = 0;
    void *p3 = sbrk(4096);
    if (p3 == (void *)-1) {
        CHECK(errno == ENOMEM, "third sbrk(+4K) failed with ENOMEM");
        brk(base);
        return;
    }
    CHECK(p3 == (char *)base + 2 * 4096, "third sbrk(+4K) returns base+8K");

    /* Write across full 12K region */
    memset(base, 0xBB, 3 * 4096);
    CHECK(((unsigned char *)base)[0] == 0xBB, "memory write succeeds");

    /* Restore */
    errno = 0;
    int ret = brk(base);
    CHECK(ret == 0, "brk(base) restores original");
}

static void test_libc_sbrk_huge_negative(void)
{
    void *before = sbrk(0);
    CHECK(before != (void *)-1, "sbrk(0) returns current break");

    errno = 0;
    void *after = sbrk(-((intptr_t)1 << 40));

    if (after == (void *)-1) {
        CHECK(errno == ENOMEM, "sbrk(huge negative) returns -1 with ENOMEM");
    } else {
        /* Some implementations clamp; verify break stays sane */
        void *current = sbrk(0);
        CHECK(current != (void *)-1, "sbrk(0) still valid");
        CHECK((uintptr_t)current <= (uintptr_t)before, "break not increased");
    }
}

static void test_libc_brk_enomem_huge(void)
{
    void *absurd = (void *)(1UL << 50);

    errno = 0;
    int ret = brk(absurd);
    CHECK(ret == -1, "brk(absurd address) returns -1");
    CHECK(errno == ENOMEM, "brk(absurd address) sets ENOMEM");

    /* Break unchanged */
    void *current = sbrk(0);
    CHECK(current != (void *)-1, "sbrk(0) still valid");
    CHECK(current != absurd, "break not moved to absurd address");
}

/* ============================================================
 * RLIMIT_DATA TESTS
 * ============================================================ */

static void test_raw_brk_rlimit_data(void)
{
    struct rlimit old_rlim;
    CHECK(getrlimit(RLIMIT_DATA, &old_rlim) == 0, "getrlimit(RLIMIT_DATA) succeeds");

    unsigned long current = syscall(SYS_brk, 0);
    CHECK(current > 0, "get current break for rlimit test");

    /* Set tight limit: current + 4K */
    struct rlimit new_rlim = {
        .rlim_cur = 4096,
        .rlim_max = old_rlim.rlim_max
    };
    CHECK(setrlimit(RLIMIT_DATA, &new_rlim) == 0, "setrlimit(RLIMIT_DATA) succeeds");

    /* Try to allocate well beyond limit */
    errno = 0;
    unsigned long beyond_limit = current + 64 * 1024;
    unsigned long ret = syscall(SYS_brk, beyond_limit);

    CHECK(ret == current, "brk beyond RLIMIT_DATA returns current break");
    CHECK(errno == 0, "raw syscall does not set errno");

    unsigned long after = syscall(SYS_brk, 0);
    CHECK(after == current, "break unchanged after rlimit rejection");

    /* Restore original limit */
    CHECK(setrlimit(RLIMIT_DATA, &old_rlim) == 0, "restore RLIMIT_DATA");
}

static void test_libc_brk_rlimit_data(void)
{
    struct rlimit old_rlim;
    CHECK(getrlimit(RLIMIT_DATA, &old_rlim) == 0, "getrlimit(RLIMIT_DATA) succeeds");

    void *original = sbrk(0);
    CHECK(original != (void *)-1, "sbrk(0) returns current break");

    /* Set tight limit: 4K */
    struct rlimit new_rlim = {
        .rlim_cur = 4096,
        .rlim_max = old_rlim.rlim_max
    };
    CHECK(setrlimit(RLIMIT_DATA, &new_rlim) == 0, "setrlimit(RLIMIT_DATA) succeeds");

    /* Try to allocate beyond limit via libc brk() */
    errno = 0;
    int ret = brk((char *)original + 64 * 1024);

    CHECK(ret == -1, "brk beyond RLIMIT_DATA returns -1");
    CHECK(errno == ENOMEM, "brk beyond RLIMIT_DATA sets ENOMEM");

    /* Break unchanged */
    CHECK(sbrk(0) == original, "break unchanged after rlimit rejection");

    /* Restore original limit */
    CHECK(setrlimit(RLIMIT_DATA, &old_rlim) == 0, "restore RLIMIT_DATA");
}

/* ============================================================
 * MAIN
 * ============================================================ */

int main(void)
{
    TEST_START("brk syscall");

    printf("--- raw syscall tests ---\n\n");

    test_raw_brk_query();
    test_raw_brk_expand_success();
    test_raw_brk_shrink_success();
    test_raw_brk_failure();
    test_raw_brk_below_base();
    test_raw_brk_roundtrip();
    test_raw_brk_rlimit_data();

    printf("\n--- libc wrapper tests ---\n\n");

    test_libc_brk_current_break();
    test_libc_sbrk_zero();
    test_libc_sbrk_allocate();
    test_libc_sbrk_sequential();
    test_libc_sbrk_huge_negative();
    test_libc_brk_enomem_huge();
    test_libc_brk_rlimit_data();

    TEST_DONE();
}