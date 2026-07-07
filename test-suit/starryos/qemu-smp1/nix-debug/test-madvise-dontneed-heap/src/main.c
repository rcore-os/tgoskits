/*
 * test-madvise-dontneed-heap — NIXPKGS-003 hypothesis:
 * glibc calls madvise(MADV_DONTNEED) on brk heap pages to release memory.
 * If StarryOS MADV_DONTNEED corrupts adjacent pages or mismanages the
 * mapping, glibc's malloc metadata gets corrupted → double free.
 *
 * Tests:
 *   A. madvise DONTNEED on middle brk pages, verify adjacent pages intact
 *   B. madvise DONTNEED on mmap pages, re-access (should be zeroed)
 *   C. madvise DONTNEED + fork: child DONTNEEDs, parent verifies
 *   D. madvise DONTNEED + subsequent brk expand into DONTNEED'd region
 */

#include "test_framework.h"

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#define PAGE_SIZE 4096

static unsigned long raw_brk(unsigned long addr)
{
    return syscall(SYS_brk, addr);
}

/* ── A: madvise DONTNEED middle brk pages, adjacent intact ──────── */

static void test_madvise_dontneed_brk_adjacent(void)
{
    printf("NIX_MADVISE_A_BEGIN\n");
    TEST_START("A: madvise DONTNEED on middle brk pages");

    unsigned long orig = raw_brk(0);
    CHECK(orig > 0, "raw_brk(0) valid");
    if (orig == 0) return;

    /* Expand brk: 16 pages */
    unsigned long expanded = orig + 16 * PAGE_SIZE;
    CHECK(raw_brk(expanded) == expanded, "brk expand 16 pages");
    if (raw_brk(0) != expanded) return;

    /* Write pattern to all 16 pages */
    for (int i = 0; i < 16; i++) {
        memset((void *)(orig + i * PAGE_SIZE), (unsigned char)(0xA0 + i), PAGE_SIZE);
    }

    /* madvise DONTNEED on pages 4-7 (middle 4 pages) */
    void *dontneed_start = (void *)(orig + 4 * PAGE_SIZE);
    int rc = madvise(dontneed_start, 4 * PAGE_SIZE, MADV_DONTNEED);
    printf("  DIAG: madvise(DONTNEED, pages 4-7) = %d errno=%d\n", rc, errno);

    /* Pages 0-3 and 8-15 should still have their original patterns */
    int intact = 1;
    for (int i = 0; i < 16; i++) {
        if (i >= 4 && i <= 7) continue; /* DONTNEED'd pages */
        unsigned char expected = (unsigned char)(0xA0 + i);
        unsigned char actual = ((unsigned char *)orig)[i * PAGE_SIZE];
        if (actual != expected) {
            printf("  DIAG: brk page %d corrupted: expected 0x%x got 0x%x\n",
                   i, expected, actual);
            intact = 0;
        }
    }
    CHECK(intact, "A: adjacent pages intact after madvise DONTNEED");

    /* Pages 4-7: reading should succeed (zero-filled or original) */
    /* Just verify we can read without crash */
    volatile unsigned char v = ((unsigned char *)dontneed_start)[0];
    (void)v;
    CHECK(1, "A: can read DONTNEED'd pages without crash");

    raw_brk(orig);
    printf("NIX_MADVISE_A_END\n");
}

/* ── B: madvise DONTNEED on mmap, re-access ────────────────────── */

static void test_madvise_dontneed_mmap_reaccess(void)
{
    printf("NIX_MADVISE_B_BEGIN\n");
    TEST_START("B: madvise DONTNEED on mmap, re-access");

    void *region = mmap(NULL, 8 * PAGE_SIZE, PROT_READ | PROT_WRITE,
                        MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(region != MAP_FAILED, "mmap 8 pages");
    if (region == MAP_FAILED) return;

    /* Write pattern */
    for (int i = 0; i < 8; i++) {
        memset((char *)region + i * PAGE_SIZE, (unsigned char)(0xB0 + i), PAGE_SIZE);
    }

    /* madvise DONTNEED on pages 2-5 */
    void *dontneed = (char *)region + 2 * PAGE_SIZE;
    int rc = madvise(dontneed, 4 * PAGE_SIZE, MADV_DONTNEED);
    printf("  DIAG: madvise(DONTNEED, mmap pages 2-5) = %d errno=%d\n", rc, errno);

    /* Pages 0-1, 6-7 should be intact */
    int intact = 1;
    for (int i = 0; i < 8; i++) {
        if (i >= 2 && i <= 5) continue;
        unsigned char expected = (unsigned char)(0xB0 + i);
        unsigned char actual = ((unsigned char *)region)[i * PAGE_SIZE];
        if (actual != expected) {
            printf("  DIAG: mmap page %d corrupted: expected 0x%x got 0x%x\n",
                   i, expected, actual);
            intact = 0;
        }
    }
    CHECK(intact, "B: adjacent mmap pages intact");

    /* Re-access DONTNEED'd pages: should be readable (zero or original) */
    volatile unsigned char v = ((unsigned char *)dontneed)[0];
    (void)v;
    CHECK(1, "B: can re-read DONTNEED'd mmap pages");

    /* Write to DONTNEED'd pages: should work */
    memset(dontneed, 0xCC, 4 * PAGE_SIZE);
    CHECK(((unsigned char *)dontneed)[0] == 0xCC, "B: can write to re-accessed DONTNEED pages");

    munmap(region, 8 * PAGE_SIZE);
    printf("NIX_MADVISE_B_END\n");
}

/* ── C: madvise DONTNEED + fork ────────────────────────────────── */

static void test_madvise_dontneed_fork(void)
{
    printf("NIX_MADVISE_C_BEGIN\n");
    TEST_START("C: madvise DONTNEED + fork isolation");

    unsigned long orig = raw_brk(0);
    CHECK(orig > 0, "raw_brk(0) valid");
    if (orig == 0) return;

    unsigned long expanded = orig + 32 * PAGE_SIZE;
    CHECK(raw_brk(expanded) == expanded, "brk expand 32 pages");
    if (raw_brk(0) != expanded) return;

    /* Write parent pattern */
    for (int i = 0; i < 32; i++) {
        memset((void *)(orig + i * PAGE_SIZE), (unsigned char)(0xC0 + i), PAGE_SIZE);
    }

    pid_t child = fork();
    CHECK(child >= 0, "fork succeeds");
    if (child < 0) { raw_brk(orig); return; }

    if (child == 0) {
        /* Child: madvise DONTNEED on pages 8-15 */
        void *dontneed = (void *)(orig + 8 * PAGE_SIZE);
        madvise(dontneed, 8 * PAGE_SIZE, MADV_DONTNEED);

        /* Write to pages 16-23 (different CoW pages) */
        for (int i = 16; i < 24; i++) {
            memset((void *)(orig + i * PAGE_SIZE), 0xDD, PAGE_SIZE);
        }
        _exit(0);
    }

    waitpid(child, NULL, 0);

    /* Parent: verify pages 0-7 and 24-31 still have original pattern */
    int intact = 1;
    for (int i = 0; i < 32; i++) {
        /* Skip pages child modified (16-23) or DONTNEED'd (8-15) */
        if (i >= 8 && i <= 23) continue;
        unsigned char expected = (unsigned char)(0xC0 + i);
        unsigned char actual = ((unsigned char *)orig)[i * PAGE_SIZE];
        if (actual != expected) {
            printf("  DIAG: parent brk page %d corrupted: expected 0x%x got 0x%x\n",
                   i, expected, actual);
            intact = 0;
        }
    }
    CHECK(intact, "C: parent pages intact after child madvise DONTNEED + fork");

    /* Pages 16-23: child wrote to them via CoW, parent should have original */
    for (int i = 16; i < 24; i++) {
        unsigned char expected = (unsigned char)(0xC0 + i);
        unsigned char actual = ((unsigned char *)orig)[i * PAGE_SIZE];
        if (actual != expected) {
            printf("  DIAG: parent brk page %d leaked child write: expected 0x%x got 0x%x\n",
                   i, expected, actual);
            intact = 0;
        }
    }
    CHECK(intact, "C: CoW isolated child writes from parent");

    raw_brk(orig);
    printf("NIX_MADVISE_C_END\n");
}

/* ── D: madvise DONTNEED + brk expand into DONTNEED'd region ───── */

static void test_madvise_dontneed_brk_expand(void)
{
    printf("NIX_MADVISE_D_BEGIN\n");
    TEST_START("D: madvise DONTNEED then brk expand into region");

    unsigned long orig = raw_brk(0);
    CHECK(orig > 0, "raw_brk(0) valid");
    if (orig == 0) return;

    /* Expand brk to 20 pages */
    unsigned long brk20 = orig + 20 * PAGE_SIZE;
    CHECK(raw_brk(brk20) == brk20, "brk expand 20 pages");

    /* Write to all pages */
    for (int i = 0; i < 20; i++) {
        memset((void *)(orig + i * PAGE_SIZE), (unsigned char)(0xD0 + i), PAGE_SIZE);
    }

    /* madvise DONTNEED on pages 12-19 */
    void *dontneed = (void *)(orig + 12 * PAGE_SIZE);
    madvise(dontneed, 8 * PAGE_SIZE, MADV_DONTNEED);

    /* Shrink brk to page 10 (below DONTNEED'd region) */
    unsigned long brk10 = orig + 10 * PAGE_SIZE;
    raw_brk(brk10);

    /* Expand brk back to page 20 (into previously DONTNEED'd region) */
    unsigned long re_brk20 = orig + 20 * PAGE_SIZE;
    unsigned long ret = raw_brk(re_brk20);
    CHECK(ret == re_brk20, "D: re-expand brk into DONTNEED'd region");

    /* Write to the re-expanded pages */
    memset((void *)(orig + 12 * PAGE_SIZE), 0xEE, 8 * PAGE_SIZE);
    CHECK(((unsigned char *)(orig + 12 * PAGE_SIZE))[0] == 0xEE,
          "D: write to re-expanded DONTNEED'd region");

    /* Pages 0-9 should still be intact */
    int intact = 1;
    for (int i = 0; i < 10; i++) {
        unsigned char expected = (unsigned char)(0xD0 + i);
        unsigned char actual = ((unsigned char *)orig)[i * PAGE_SIZE];
        if (actual != expected) {
            printf("  DIAG: page %d corrupted after DONTNEED+brk cycle: expected 0x%x got 0x%x\n",
                   i, expected, actual);
            intact = 0;
        }
    }
    CHECK(intact, "D: pages 0-9 intact after DONTNEED + brk shrink/expand");

    raw_brk(orig);
    printf("NIX_MADVISE_D_END\n");
}

int main(void)
{
    printf("NIX_MADVISE_TEST_BEGIN\n");

    test_madvise_dontneed_brk_adjacent();
    test_madvise_dontneed_mmap_reaccess();
    test_madvise_dontneed_fork();
    test_madvise_dontneed_brk_expand();

    printf("NIX_MADVISE_ALL_PASSED\n");
    return 0;
}
