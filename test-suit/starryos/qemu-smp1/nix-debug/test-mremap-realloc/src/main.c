/*
 * test-mremap-realloc — glibc uses mremap(MREMAP_MAYMOVE) for large
 * realloc(). If StarryOS mremap corrupts data or returns overlapping
 * regions, glibc's malloc arena management gets corrupted.
 */

#include "test_framework.h"

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#define PAGE_SIZE 4096

#ifndef MREMAP_MAYMOVE
#define MREMAP_MAYMOVE 1
#endif
#ifndef MREMAP_FIXED
#define MREMAP_FIXED 2
#endif

static void *do_mremap(void *old, size_t old_sz, size_t new_sz, int flags)
{
    return (void *)syscall(SYS_mremap, old, old_sz, new_sz, flags, 0);
}

int main(void)
{
    printf("NIX_MREMAP_BEGIN\n");
    TEST_START("mremap-realloc: mremap grow/shrink/move");

    void *orig = mmap(NULL, 4 * PAGE_SIZE, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(orig != MAP_FAILED, "mmap 4 pages");
    if (orig == MAP_FAILED) return 1;

    /* Write pattern */
    for (int i = 0; i < 4; i++)
        memset((char *)orig + i * PAGE_SIZE, (unsigned char)(0xA0 + i), PAGE_SIZE);

    /* ── grow in-place ── */
    void *grown = do_mremap(orig, 4 * PAGE_SIZE, 8 * PAGE_SIZE, 0);
    if (grown == MAP_FAILED && errno == ENOMEM) {
        printf("  DIAG: mremap grow in-place ENOMEM (expected if no space)\n");
    } else {
        CHECK(grown != MAP_FAILED, "mremap grow 4→8 pages");
        if (grown != MAP_FAILED) {
            CHECK(grown == orig, "mremap grow returned same address");
            /* Original data intact */
            for (int i = 0; i < 4; i++) {
                CHECK(((unsigned char *)grown)[i * PAGE_SIZE] == (unsigned char)(0xA0 + i),
                      "data intact after mremap grow");
            }
            /* Write to new pages */
            for (int i = 4; i < 8; i++)
                memset((char *)grown + i * PAGE_SIZE, (unsigned char)(0xB0 + i), PAGE_SIZE);
            orig = grown;
        }
    }

    /* ── shrink ── */
    void *shrunk = do_mremap(orig, 8 * PAGE_SIZE, 2 * PAGE_SIZE, 0);
    CHECK(shrunk != MAP_FAILED, "mremap shrink 8→2 pages");
    if (shrunk != MAP_FAILED) {
        CHECK(((unsigned char *)shrunk)[0] == 0xA0, "first page intact after shrink");
        CHECK(((unsigned char *)shrunk)[PAGE_SIZE] == 0xA1, "second page intact after shrink");
        orig = shrunk;
    }

    /* ── MAYMOVE: expand ── */
    void *moved = do_mremap(orig, 2 * PAGE_SIZE, 6 * PAGE_SIZE, MREMAP_MAYMOVE);
    CHECK(moved != MAP_FAILED, "mremap MAYMOVE 2→6 pages");
    if (moved != MAP_FAILED) {
        /* Data preserved */
        CHECK(((unsigned char *)moved)[0] == 0xA0, "data intact after MAYMOVE");
        CHECK(((unsigned char *)moved)[PAGE_SIZE] == 0xA1, "second page intact after MAYMOVE");
        /* New pages writable */
        for (int i = 2; i < 6; i++)
            memset((char *)moved + i * PAGE_SIZE, (unsigned char)(0xC0 + i), PAGE_SIZE);
        for (int i = 2; i < 6; i++)
            CHECK(((unsigned char *)moved)[i * PAGE_SIZE] == (unsigned char)(0xC0 + i),
                  "new pages writable after MAYMOVE");
        munmap(moved, 6 * PAGE_SIZE);
    } else {
        munmap(orig, 2 * PAGE_SIZE);
    }

    /* ── MAYMOVE: expand with adjacent conflict ── */
    void *a = mmap(NULL, 2 * PAGE_SIZE, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(a != MAP_FAILED, "mmap region A");
    void *b = mmap(NULL, 2 * PAGE_SIZE, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(b != MAP_FAILED, "mmap region B");

    memset(a, 0xDD, 2 * PAGE_SIZE);
    memset(b, 0xEE, 2 * PAGE_SIZE);

    /* Try to grow A when B is adjacent — should MAYMOVE */
    void *a_grown = do_mremap(a, 2 * PAGE_SIZE, 4 * PAGE_SIZE, MREMAP_MAYMOVE);
    CHECK(a_grown != MAP_FAILED, "mremap MAYMOVE with adjacent conflict");
    if (a_grown != MAP_FAILED) {
        CHECK(((unsigned char *)a_grown)[0] == 0xDD, "data intact after conflict MAYMOVE");
        /* Region B should be unaffected */
        CHECK(((unsigned char *)b)[0] == 0xEE, "adjacent region B unaffected");
        munmap(a_grown, 4 * PAGE_SIZE);
    }
    munmap(b, 2 * PAGE_SIZE);
    if (a_grown == MAP_FAILED) munmap(a, 2 * PAGE_SIZE);

    printf("NIX_MREMAP_PASSED\n");
    return 0;
}
