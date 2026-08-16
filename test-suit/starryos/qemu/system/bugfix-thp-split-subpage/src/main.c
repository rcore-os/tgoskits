// Transparent-huge-page promotion + huge->4K split correctness.
//
// A large writable private-anonymous mmap is promoted to 2 MiB blocks (with the
// `thp` kernel feature). Any sub-2 MiB `mprotect`/`madvise` must split the block
// into 512 4 KiB leaves *content-preservingly* — the untouched sub-pages keep
// their data, the mprotected sub-page keeps its data at the new permission, and a
// write to another sub-page still works. This exercises the whole pipeline
// (mmap promotion carve -> first-touch 2 MiB fault -> break-before-make split ->
// per-leaf remap). With the feature off there is no promotion, so the same
// sequence runs on 4 KiB pages and still passes (a mmap/mprotect/madvise smoke).
#define _GNU_SOURCE
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define HUGE (0x200000UL) // 2 MiB

static int failed;

static void note_fail(const char *what, const char *detail)
{
    printf("FAIL: %s: %s\n", what, detail);
    failed++;
}

static unsigned char pat(size_t page_index)
{
    return (unsigned char)((page_index * 7 + 3) & 0xff);
}

int main(void)
{
    printf("=== thp-split-subpage ===\n");

    long ps_signed = sysconf(_SC_PAGESIZE);
    if (ps_signed <= 0) {
        note_fail("sysconf", strerror(errno));
        printf("SOME TESTS FAILED\n");
        return 1;
    }
    size_t ps = (size_t)ps_signed;
    size_t len = 2 * HUGE; // two 2 MiB blocks

    // Reserve len + HUGE so we can 2 MiB-align the base within the reservation:
    // a 2 MiB-aligned, >= 2 MiB writable private-anon region is THP-eligible.
    char *raw = mmap(NULL, len + HUGE, PROT_READ | PROT_WRITE,
                     MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (raw == MAP_FAILED) {
        note_fail("mmap", strerror(errno));
        printf("SOME TESTS FAILED\n");
        return 1;
    }
    char *base = (char *)(((uintptr_t)raw + HUGE - 1) & ~(HUGE - 1));
    size_t npages = len / ps;

    // First-touch every page: with THP each 2 MiB block faults in as one block.
    for (size_t i = 0; i < npages; i++) {
        base[i * ps] = pat(i);
    }

    // mprotect a middle 4 KiB sub-page of the SECOND block read-only. On a THP
    // build this forces that 2 MiB block to split into 512 4 KiB leaves.
    size_t mid_page = (HUGE / ps) + 8; // 8 pages into the 2nd block
    if (mprotect(base + mid_page * ps, ps, PROT_READ) != 0) {
        note_fail("mprotect RO", strerror(errno));
    }

    // Every sub-page must keep its first-touch data across the split.
    for (size_t i = 0; i < npages; i++) {
        if (base[i * ps] != (char)pat(i)) {
            char d[96];
            snprintf(d, sizeof(d), "page %zu: got %d want %d", i, base[i * ps],
                     (int)pat(i));
            note_fail("data preserved across split", d);
            break;
        }
    }

    // A write to another (now 4 KiB) sub-page of the split block still works.
    size_t other_page = (HUGE / ps) + 100;
    base[other_page * ps] = 0x5a;
    if (base[other_page * ps] != 0x5a) {
        note_fail("write after split", "readback mismatch");
    }

    // MADV_NOHUGEPAGE over the first (still-huge) block splits it to 4 KiB too,
    // without error, and its data survives.
    if (madvise(base, HUGE, MADV_NOHUGEPAGE) != 0) {
        note_fail("madvise NOHUGEPAGE", strerror(errno));
    }
    for (size_t i = 0; i < HUGE / ps; i++) {
        if (base[i * ps] != (char)pat(i)) {
            note_fail("data preserved across MADV_NOHUGEPAGE", "mismatch");
            break;
        }
    }

    munmap(raw, len + HUGE);

    if (failed == 0) {
        printf("ALL TESTS PASSED\n");
        return 0;
    }
    printf("SOME TESTS FAILED\n");
    return 1;
}
