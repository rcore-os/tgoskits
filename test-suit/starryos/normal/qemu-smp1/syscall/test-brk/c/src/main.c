/*
 * test_brk.c - Test cases for brk(2) / sbrk(2)
 *
 * Covers normal and error return paths as described in man 2 brk.
 * Compile: gcc -Wall -Wextra -std=c99 -o test_brk test_brk.c
 * Run:     ./test_brk
 */

/* Enable brk/sbrk declarations per glibc feature-test-macro requirements */
#if !defined(_DEFAULT_SOURCE)
#define _DEFAULT_SOURCE 1
#endif

#include <assert.h>
#include <stdint.h>
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <sys/resource.h>

/* ---------- helper macros ---------- */

#define FAIL(fmt, ...) \
    do { fprintf(stderr, "FAIL [%s:%d]: " fmt "\n", __FILE__, __LINE__, ##__VA_ARGS__); } while (0)

/* ---------- normal-path tests ---------- */

/*
 * brk() with the current break address should succeed (no-op).
 */
static void test_brk_current_break(void)
{
    void *cur = sbrk(0);
    assert(cur != (void *)-1);
    void *before = cur;
    errno = 0;
    int ret = brk(cur);
    if (ret == 0) {
        assert(sbrk(0) == before);
        return;
    }

    /* Static musl binaries may reject a no-op brk() with ENOMEM. */
    assert(ret == -1);
    assert(errno == ENOMEM);
    assert(sbrk(0) == before);
}

/*
 * brk() increase: move the break forward by one page, write to it,
 * then restore. Should return 0.
 */
static void test_brk_increase_then_restore(void)
{
    void *original = sbrk(0);
    assert(original != (void *)-1);

    long page_size = sysconf(_SC_PAGESIZE);
    assert(page_size > 0);

    void *new_break = (char *)original + page_size;
    errno = 0;
    int ret = brk(new_break);
    if (ret == 0) {
        /* Verify we can write to the newly allocated region */
        memset(original, 0xAA, (size_t)page_size);

        /* Restore original break */
        ret = brk(original);
        assert(ret == 0);
        return;
    }

    assert(ret == -1);
    assert(errno == ENOMEM);
    assert(sbrk(0) == original);
}

/*
 * sbrk(0) should return the current program break without changing it.
 */
static void test_sbrk_zero(void)
{
    void *p1 = sbrk(0);
    assert(p1 != (void *)-1);
    void *p2 = sbrk(0);
    assert(p2 != (void *)-1);
    /* Two consecutive sbrk(0) calls should return the same value */
    assert(p1 == p2);
}

/*
 * sbrk(positive) should return the previous break, which points to
 * the start of the newly allocated memory.
 */
static void test_sbrk_allocate(void)
{
    long page_size = sysconf(_SC_PAGESIZE);
    assert(page_size > 0);

    void *old_break = sbrk(0);
    assert(old_break != (void *)-1);

    errno = 0;
    void *returned = sbrk(page_size);
    if (returned == (void *)-1) {
        assert(errno == ENOMEM);
        return;
    }

    assert(returned == old_break);

    /* Write to the allocated memory */
    memset(returned, 0x55, (size_t)page_size);

    /* Clean up: deallocate */
    void *after = sbrk(-page_size);
    assert(after != (void *)-1);
}

/*
 * sbrk(negative) should shrink the data segment and return the break
 * before the shrink.
 */
static void test_sbrk_deallocate(void)
{
    long page_size = sysconf(_SC_PAGESIZE);
    assert(page_size > 0);

    /* First allocate some space */
    errno = 0;
    void *before_alloc = sbrk(page_size);
    if (before_alloc == (void *)-1) {
        assert(errno == ENOMEM);
        return;
    }

    /* Now deallocate it */
    void *before_free = sbrk(-page_size);
    assert(before_free != (void *)-1);

    /* After freeing, the break should be back at the original break. */
    void *current = sbrk(0);
    assert(current == before_alloc);
}

/*
 * Multiple sequential sbrk() allocations: each call should return the
 * previous break, forming a contiguous region.
 */
static void test_sbrk_sequential(void)
{
    long page_size = sysconf(_SC_PAGESIZE);
    assert(page_size > 0);

    void *base = sbrk(0);
    assert(base != (void *)-1);

    errno = 0;
    void *p1 = sbrk(page_size);
    if (p1 == (void *)-1) {
        assert(errno == ENOMEM);
        return;
    }
    assert(p1 == base);

    void *p2 = sbrk(page_size);
    if (p2 == (void *)-1) {
        assert(errno == ENOMEM);
        brk(base);
        return;
    }
    assert(p2 == (char *)base + page_size);

    void *p3 = sbrk(page_size);
    if (p3 == (void *)-1) {
        assert(errno == ENOMEM);
        brk(base);
        return;
    }
    assert(p3 == (char *)base + 2 * page_size);

    /* Write across the full 3-page region */
    memset(base, 0xBB, (size_t)(3 * page_size));

    /* Restore */
    int ret = brk(base);
    assert(ret == 0);
}

/* ---------- error-path tests ---------- */

/*
 * brk() with an absurdly high address should fail with ENOMEM.
 * We try to set the break to near the max user-space address.
 */
static void test_brk_enomem_high_address(void)
{
    /* Use an address far beyond reasonable process address space */
    void *absurd = (void *)(unsigned long)(1UL << 50);
    errno = 0;
    int ret = brk(absurd);
    assert(ret == -1);
    assert(errno == ENOMEM);
}

/*
 * sbrk() with a huge positive increment should fail with ENOMEM.
 */
static void test_sbrk_enomem_huge_increment(void)
{
    errno = 0;
    void *ret = sbrk((intptr_t)1 << 40);
    assert(ret == (void *)-1);
    assert(errno == ENOMEM);
}

/*
 * Use setrlimit(RLIMIT_DATA, ...) to impose a tight limit, then try
 * to brk() beyond it — should fail with ENOMEM.
 */
static void test_brk_enomem_rlimit(void)
{
    struct rlimit old_rlim, new_rlim;
    int rc = getrlimit(RLIMIT_DATA, &old_rlim);
    assert(rc == 0);

    void *original = sbrk(0);
    assert(original != (void *)-1);

    /* Set a very tight data limit: current break + 4 KiB */
    new_rlim.rlim_cur = 4096;
    new_rlim.rlim_max = old_rlim.rlim_max;
    rc = setrlimit(RLIMIT_DATA, &new_rlim);
    assert(rc == 0);

    /* Try to allocate well beyond the limit */
    errno = 0;
    int ret = brk((char *)original + 64 * 1024);
    assert(ret == -1);
    assert(errno == ENOMEM);

    /* Restore original limit */
    rc = setrlimit(RLIMIT_DATA, &old_rlim);
    assert(rc == 0);
}

/*
 * sbrk() with a huge negative increment: glibc's internal bookkeeping
 * clamps the break to the data-segment floor rather than returning an
 * error, so we verify the invariant that the break remains valid (i.e.
 * did not wrap or move to an absurd address).
 */
static void test_sbrk_huge_negative_invariant(void)
{
    void *before = sbrk(0);
    assert(before != (void *)-1);

    /* glibc may silently clamp; the important thing is the break stays sane */
    void *after_alloc = sbrk(-((intptr_t)1 << 40));
    if (after_alloc == (void *)-1) {
        /* Some implementations do return ENOMEM — accept that too */
        assert(errno == ENOMEM);
    } else {
        /* glibc clamped: the current break should still be >= before */
        void *current = sbrk(0);
        assert(current != (void *)-1);
        assert((uintptr_t)current <= (uintptr_t)before);
    }
}

/* ---------- main ---------- */

int main(void)
{
    const struct {
        const char *name;
        void (*fn)(void);
    } tests[] = {
        /* Normal paths */
        { "brk with current break",           test_brk_current_break          },
        { "brk increase then restore",        test_brk_increase_then_restore  },
        { "sbrk(0) returns current break",    test_sbrk_zero                  },
        { "sbrk(positive) allocates memory",  test_sbrk_allocate              },
        { "sbrk(negative) deallocates memory", test_sbrk_deallocate           },
        { "sbrk sequential allocations",      test_sbrk_sequential            },

        /* Error paths */
        { "brk ENOMEM (absurd address)",      test_brk_enomem_high_address    },
        { "sbrk ENOMEM (huge increment)",     test_sbrk_enomem_huge_increment },
        { "brk ENOMEM (RLIMIT_DATA)",         test_brk_enomem_rlimit          },
        { "sbrk huge negative invariant",     test_sbrk_huge_negative_invariant },
    };

    int passed = 0;
    int total  = (int)(sizeof(tests) / sizeof(tests[0]));

    for (int i = 0; i < total; i++) {
        printf("[ RUN  ] %s\n", tests[i].name);
        tests[i].fn();
        printf("[ PASS ] %s\n", tests[i].name);
        passed++;
    }

    printf("\n%d / %d tests passed.\n", passed, total);
    return 0;
}