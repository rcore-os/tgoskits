/*
 * test_brk.c - Test cases for brk(2) / sbrk(2)
 *
 * Covers raw syscall and libc wrapper semantics.
 * Separate test groups to avoid state contamination.
 */

#if !defined(_DEFAULT_SOURCE)
#define _DEFAULT_SOURCE 1
#endif

#include <assert.h>
#include <stdint.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>
#include <sys/syscall.h>

/* ============================================================
 * RAW SYSCALL TESTS - Verify Linux brk syscall ABI directly
 * ============================================================ */

static void test_raw_brk_query(void)
{
    unsigned long break1 = syscall(SYS_brk, 0);
    assert(break1 > 0);

    unsigned long break2 = syscall(SYS_brk, 0);
    assert(break2 == break1);

    printf("  raw brk(0) = 0x%lx\n", break1);
}

static void test_raw_brk_expand_success(void)
{
    unsigned long current = syscall(SYS_brk, 0);
    assert(current > 0);

    unsigned long new_addr = current + 4096;
    unsigned long ret = syscall(SYS_brk, new_addr);

    /* MUST return new_addr on success */
    assert(ret == new_addr);

    unsigned long after = syscall(SYS_brk, 0);
    assert(after == new_addr);

    /* Write to memory */
    memset((void *)current, 0xAA, 4096);
    assert(((unsigned char *)current)[0] == 0xAA);

    /* Restore */
    syscall(SYS_brk, current);

    after = syscall(SYS_brk, 0);
    assert(after == current);
}

static void test_raw_brk_shrink_success(void)
{
    unsigned long current = syscall(SYS_brk, 0);

    unsigned long expanded = current + 8192;
    unsigned long ret = syscall(SYS_brk, expanded);
    assert(ret == expanded);

    ret = syscall(SYS_brk, current);
    assert(ret == current);

    unsigned long after = syscall(SYS_brk, 0);
    assert(after == current);
}

static void test_raw_brk_failure(void)
{
    unsigned long current = syscall(SYS_brk, 0);
    assert(current > 0);

    unsigned long absurd = 1UL << 50;
    errno = 0;
    unsigned long ret = syscall(SYS_brk, absurd);

    assert(ret == current);
    assert(errno == 0);

    unsigned long after = syscall(SYS_brk, 0);
    assert(after == current);
}

static void test_raw_brk_below_base(void)
{
    unsigned long current = syscall(SYS_brk, 0);
    assert(current > 0);

    errno = 0;
    unsigned long ret = syscall(SYS_brk, 0x1000);

    assert(ret == current);
    assert(errno == 0);

    unsigned long after = syscall(SYS_brk, 0);
    assert(after == current);
}

/* ============================================================
 * LIBC WRAPPER TESTS - Use ONLY libc functions
 * These tests run in a fresh process (forked) to avoid contamination
 * ============================================================ */

static void run_libc_tests(void)
{
    /* Test 1: sbrk(0) */
    void *p1 = sbrk(0);
    assert(p1 != (void *)-1);
    void *p2 = sbrk(0);
    assert(p2 == p1);
    printf("[ PASS ] libc sbrk(0)\n\n");

    /* Test 2: brk expand */
    void *original = sbrk(0);
    void *new_break = (char *)original + 4096;
    errno = 0;
    int ret = brk(new_break);
    assert(ret == 0);
    void *after = sbrk(0);
    assert(after == new_break);
    memset(original, 0x55, 4096);
    assert(((unsigned char *)original)[0] == 0x55);
    brk(original);
    after = sbrk(0);
    assert(after == original);
    printf("[ PASS ] libc brk expand MUST succeed\n\n");

    /* Test 3: sbrk expand */
    void *old_break = sbrk(0);
    errno = 0;
    void *returned = sbrk(4096);
    assert(returned == old_break);
    memset(returned, 0xBB, 4096);
    sbrk(-4096);
    void *final = sbrk(0);
    assert(final == old_break);
    printf("[ PASS ] libc sbrk expand MUST succeed\n\n");

    /* Test 4: brk failure */
    void *current = sbrk(0);
    void *absurd = (void *)(1UL << 50);
    errno = 0;
    ret = brk(absurd);
    assert(ret == -1);
    assert(errno == ENOMEM);
    after = sbrk(0);
    assert(after == current);
    printf("[ PASS ] libc brk failure ENOMEM\n\n");

    /* Test 5: sbrk failure */
    current = sbrk(0);
    errno = 0;
    void *sbrk_ret = sbrk((intptr_t)1 << 40);
    assert(sbrk_ret == (void *)-1);
    assert(errno == ENOMEM);
    after = sbrk(0);
    assert(after == current);
    printf("[ PASS ] libc sbrk failure ENOMEM\n\n");

    /* Test 6: sequential sbrk */
    void *base = sbrk(0);
    errno = 0;
    void *p3_1 = sbrk(4096);
    assert(p3_1 == base);
    void *p3_2 = sbrk(4096);
    assert(p3_2 == (char *)base + 4096);
    void *p3_3 = sbrk(4096);
    assert(p3_3 == (char *)base + 8192);
    memset(base, 0xCC, 12288);
    assert(((unsigned char *)base)[0] == 0xCC);
    brk(base);
    after = sbrk(0);
    assert(after == base);
    printf("[ PASS ] libc sbrk sequential MUST ok\n\n");
}

/* Fork to run libc tests in a clean process */
static void test_libc_wrapper_forked(void)
{
    pid_t pid = fork();
    if (pid == 0) {
        /* Child: run libc tests */
        run_libc_tests();
        exit(0);
    } else {
        /* Parent: wait for child */
        int status;
        waitpid(pid, &status, 0);
        assert(WIFEXITED(status));
        assert(WEXITSTATUS(status) == 0);
    }
}

/* ============================================================
 * MAIN
 * ============================================================ */

int main(void)
{
    const struct {
        const char *name;
        void (*fn)(void);
    } raw_tests[] = {
        { "raw brk(0) query",            test_raw_brk_query            },
        { "raw brk expand MUST succeed", test_raw_brk_expand_success   },
        { "raw brk shrink MUST succeed", test_raw_brk_shrink_success   },
        { "raw brk failure returns cur", test_raw_brk_failure          },
        { "raw brk below base",          test_raw_brk_below_base       },
    };

    int passed = 0;
    int raw_total = (int)(sizeof(raw_tests) / sizeof(raw_tests[0]));

    printf("=== brk/sbrk syscall tests ===\n\n");
    printf("--- raw syscall tests ---\n\n");

    for (int i = 0; i < raw_total; i++) {
        printf("[ RUN  ] %s\n", raw_tests[i].name);
        raw_tests[i].fn();
        printf("[ PASS ] %s\n\n", raw_tests[i].name);
        passed++;
    }

    printf("--- libc wrapper tests (forked for isolation) ---\n\n");
    printf("[ RUN  ] libc wrapper tests\n");
    test_libc_wrapper_forked();
    printf("[ PASS ] libc wrapper tests (6 tests)\n\n");
    passed++;

    printf("=== %d test groups passed ===\n", passed);
    return 0;
}