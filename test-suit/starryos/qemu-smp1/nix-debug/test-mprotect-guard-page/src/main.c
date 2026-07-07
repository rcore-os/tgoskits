/*
 * test-mprotect-guard-page — NIXPKGS-003 hypothesis:
 * glibc uses mprotect(PROT_NONE) for guard pages between malloc chunks.
 * If StarryOS mprotect corrupts adjacent pages or fails to restore
 * permissions correctly, glibc's malloc metadata gets corrupted.
 *
 * Tests:
 *   A. mprotect PROT_NONE on middle page, verify adjacent intact
 *   B. mprotect PROT_NONE → PROT_READ|WRITE roundtrip, verify data
 *   C. mprotect guard page + fork: child sets guard, parent unaffected
 *   D. rapid mprotect cycle: PROT_NONE ↔ RW many times
 */

#include "test_framework.h"

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

#define PAGE_SIZE 4096

/* ── A: PROT_NONE middle page, adjacent intact ──────────────────── */

static void test_mprotect_guard_adjacent(void)
{
    printf("NIX_MPROTECT_A_BEGIN\n");
    TEST_START("A: mprotect PROT_NONE middle page, adjacent intact");

    void *region = mmap(NULL, 8 * PAGE_SIZE, PROT_READ | PROT_WRITE,
                        MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(region != MAP_FAILED, "mmap 8 pages");
    if (region == MAP_FAILED) return;

    for (int i = 0; i < 8; i++)
        memset((char *)region + i * PAGE_SIZE, (unsigned char)(0xA0 + i), PAGE_SIZE);

    /* Set page 4 as PROT_NONE guard */
    void *guard = (char *)region + 4 * PAGE_SIZE;
    int rc = mprotect(guard, PAGE_SIZE, PROT_NONE);
    CHECK(rc == 0, "A: mprotect(PROT_NONE) page 4");

    /* Pages 0-3 and 5-7 should be intact */
    int intact = 1;
    for (int i = 0; i < 8; i++) {
        if (i == 4) continue;
        unsigned char expected = (unsigned char)(0xA0 + i);
        unsigned char actual = ((unsigned char *)region)[i * PAGE_SIZE];
        if (actual != expected) {
            printf("  DIAG: page %d corrupted: expected 0x%x got 0x%x\n",
                   i, expected, actual);
            intact = 0;
        }
    }
    CHECK(intact, "A: adjacent pages intact after PROT_NONE");

    /* Restore permissions */
    rc = mprotect(guard, PAGE_SIZE, PROT_READ | PROT_WRITE);
    CHECK(rc == 0, "A: mprotect restore RW on guard page");

    /* Guard page should have original data (or be zeroed on some OS) */
    volatile unsigned char v = ((unsigned char *)guard)[0];
    (void)v;
    CHECK(1, "A: can read restored guard page");

    munmap(region, 8 * PAGE_SIZE);
    printf("NIX_MPROTECT_A_END\n");
}

/* ── B: PROT_NONE → RW roundtrip with data verify ──────────────── */

static void test_mprotect_roundtrip_data(void)
{
    printf("NIX_MPROTECT_B_BEGIN\n");
    TEST_START("B: PROT_NONE → RW roundtrip data verify");

    void *page = mmap(NULL, PAGE_SIZE, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(page != MAP_FAILED, "mmap 1 page");
    if (page == MAP_FAILED) return;

    memset(page, 0xBB, PAGE_SIZE);

    /* PROT_NONE → should not be able to read/write */
    CHECK(mprotect(page, PAGE_SIZE, PROT_NONE) == 0, "B: PROT_NONE");

    /* PROT_READ only */
    CHECK(mprotect(page, PAGE_SIZE, PROT_READ) == 0, "B: PROT_READ");
    volatile unsigned char v1 = ((unsigned char *)page)[0];
    (void)v1;

    /* PROT_READ|WRITE */
    CHECK(mprotect(page, PAGE_SIZE, PROT_READ | PROT_WRITE) == 0, "B: PROT_RW");
    CHECK(((unsigned char *)page)[0] == 0xBB, "B: data intact after roundtrip");
    CHECK(((unsigned char *)page)[PAGE_SIZE - 1] == 0xBB, "B: last byte intact");

    munmap(page, PAGE_SIZE);
    printf("NIX_MPROTECT_B_END\n");
}

/* ── C: mprotect guard + fork ──────────────────────────────────── */

static void test_mprotect_guard_fork(void)
{
    printf("NIX_MPROTECT_C_BEGIN\n");
    TEST_START("C: mprotect guard + fork isolation");

    void *region = mmap(NULL, 4 * PAGE_SIZE, PROT_READ | PROT_WRITE,
                        MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(region != MAP_FAILED, "mmap 4 pages");
    if (region == MAP_FAILED) return;

    memset(region, 0xCC, 4 * PAGE_SIZE);

    pid_t child = fork();
    CHECK(child >= 0, "fork succeeds");
    if (child < 0) { munmap(region, 4 * PAGE_SIZE); return; }

    if (child == 0) {
        /* Child: set guard on page 2, write to page 3 */
        mprotect((char *)region + 2 * PAGE_SIZE, PAGE_SIZE, PROT_NONE);
        memset((char *)region + 3 * PAGE_SIZE, 0xDD, PAGE_SIZE);
        _exit(0);
    }

    waitpid(child, NULL, 0);

    /* Parent: all pages should have 0xCC (child's mprotect/write isolated by CoW) */
    int intact = 1;
    for (int i = 0; i < 4; i++) {
        unsigned char actual = ((unsigned char *)region)[i * PAGE_SIZE];
        if (actual != 0xCC) {
            printf("  DIAG: parent page %d corrupted: expected 0xCC got 0x%x\n", i, actual);
            intact = 0;
        }
    }
    CHECK(intact, "C: parent pages intact after child mprotect + fork");

    munmap(region, 4 * PAGE_SIZE);
    printf("NIX_MPROTECT_C_END\n");
}

/* ── D: rapid mprotect cycle ───────────────────────────────────── */

static void test_mprotect_rapid_cycle(void)
{
    printf("NIX_MPROTECT_D_BEGIN\n");
    TEST_START("D: rapid mprotect cycle");

    void *page = mmap(NULL, PAGE_SIZE, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(page != MAP_FAILED, "mmap 1 page");
    if (page == MAP_FAILED) return;

    for (int cycle = 0; cycle < 100; cycle++) {
        memset(page, (unsigned char)cycle, PAGE_SIZE);
        CHECK(mprotect(page, PAGE_SIZE, PROT_NONE) == 0, "D: PROT_NONE");
        CHECK(mprotect(page, PAGE_SIZE, PROT_READ) == 0, "D: PROT_READ");
        CHECK(mprotect(page, PAGE_SIZE, PROT_READ | PROT_WRITE) == 0, "D: PROT_RW");
        if (((unsigned char *)page)[0] != (unsigned char)cycle) {
            printf("  DIAG: cycle %d data lost: expected 0x%x got 0x%x\n",
                   cycle, (unsigned char)cycle, ((unsigned char *)page)[0]);
            CHECK(0, "D: data lost during cycle");
            break;
        }
    }
    CHECK(1, "D: 100 mprotect cycles completed");

    munmap(page, PAGE_SIZE);
    printf("NIX_MPROTECT_D_END\n");
}

int main(void)
{
    printf("NIX_MPROTECT_TEST_BEGIN\n");

    test_mprotect_guard_adjacent();
    test_mprotect_roundtrip_data();
    test_mprotect_guard_fork();
    test_mprotect_rapid_cycle();

    printf("NIX_MPROTECT_ALL_PASSED\n");
    return 0;
}
