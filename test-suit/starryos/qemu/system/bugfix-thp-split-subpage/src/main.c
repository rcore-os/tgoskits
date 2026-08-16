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
#include <sys/wait.h>
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

// After fork, a promoted 2 MiB block is COW-shared (refcount 2). A sub-2 MiB op in
// the child must COW-break it — copy the block into a private frame, drop the shared
// ref, register 512 fresh 4 KiB refcounts, and re-map — then split to 4 KiB, all
// without disturbing the parent's still-2 MiB view. This is the path
// (`prepare_huge_split_2m` CopiedContiguous / CopiedScattered) the same-process
// split cannot reach; a refcount or isolation bug here surfaces only after fork.
static void fork_cow_split_isolation(void)
{
    long ps_signed = sysconf(_SC_PAGESIZE);
    if (ps_signed <= 0) {
        note_fail("sysconf (cow)", strerror(errno));
        return;
    }
    size_t ps = (size_t)ps_signed;
    size_t npages = HUGE / ps;

    char *raw = mmap(NULL, HUGE + HUGE, PROT_READ | PROT_WRITE,
                     MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (raw == MAP_FAILED) {
        note_fail("mmap (cow)", strerror(errno));
        return;
    }
    char *base = (char *)(((uintptr_t)raw + HUGE - 1) & ~(HUGE - 1));
    for (size_t i = 0; i < npages; i++) {
        base[i * ps] = pat(i); // first-touch -> one private 2 MiB block
    }

    pid_t pid = fork(); // now the 2 MiB block is COW-shared between parent + child
    if (pid < 0) {
        note_fail("fork", strerror(errno));
        munmap(raw, HUGE + HUGE);
        return;
    }
    if (pid == 0) {
        // Child: mprotect a sub-page RO -> splits the SHARED block (COW-break to a
        // private copy, then 4 KiB leaves). Then write a child-only marker.
        size_t wp = 50;
        if (mprotect(base + 8 * ps, ps, PROT_READ) != 0) {
            _exit(11);
        }
        base[wp * ps] = 0x33; // child-private write after the split
        for (size_t i = 0; i < npages; i++) {
            if (i == wp) {
                continue;
            }
            if (base[i * ps] != (char)pat(i)) {
                _exit(12); // data lost across the COW-break split
            }
        }
        _exit(base[wp * ps] == 0x33 ? 0 : 13);
    }

    int st = 0;
    waitpid(pid, &st, 0);
    if (!(WIFEXITED(st) && WEXITSTATUS(st) == 0)) {
        char d[64];
        snprintf(d, sizeof(d), "child status=%d", st);
        note_fail("child COW-break split", d);
    }
    // Parent isolation: the child's private COW-break + write must not have touched
    // the parent's block.
    for (size_t i = 0; i < npages; i++) {
        if (base[i * ps] != (char)pat(i)) {
            char d[64];
            snprintf(d, sizeof(d), "parent page %zu changed to %d", i, base[i * ps]);
            note_fail("parent isolation after child COW split", d);
            break;
        }
    }
    munmap(raw, HUGE + HUGE);
}

// mremap on a THP-promoted region must keep Linux's 4 KiB ABI. A 4 KiB
// MREMAP_FIXED move of a promoted block's head to a 4 KiB-aligned (but not
// 2 MiB-aligned) target must succeed, move only that 4 KiB (preserving its data),
// and leave the rest of the source block in place. Without the split-on-mremap
// fix the area's 2 MiB backend page_size forces 2 MiB granularity, so this either
// EINVALs (target not 2 MiB-aligned) or moves the whole 2 MiB block.
static void mremap_4k_of_promoted_block_keeps_4k_abi(void)
{
    long ps_signed = sysconf(_SC_PAGESIZE);
    if (ps_signed <= 0) {
        note_fail("sysconf (mremap)", strerror(errno));
        return;
    }
    size_t ps = (size_t)ps_signed;

    // 2 MiB-aligned, >= 2 MiB writable private-anon region -> THP-promoted body.
    char *raw = mmap(NULL, HUGE + HUGE, PROT_READ | PROT_WRITE,
                     MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (raw == MAP_FAILED) {
        note_fail("mmap (mremap)", strerror(errno));
        return;
    }
    char *base = (char *)(((uintptr_t)raw + HUGE - 1) & ~(HUGE - 1));
    base[0] = 0x71;  // first-touch the promoted block
    base[ps] = 0x72; // its second 4 KiB page

    // Reserve a scratch region and pick a 4 KiB-aligned, NON-2 MiB-aligned target.
    char *scratch = mmap(NULL, HUGE + HUGE, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (scratch == MAP_FAILED) {
        note_fail("mmap scratch (mremap)", strerror(errno));
        munmap(raw, HUGE + HUGE);
        return;
    }
    char *target =
        (char *)((((uintptr_t)scratch + HUGE - 1) & ~(HUGE - 1)) + ps);

    void *ret = mremap(base, ps, ps, MREMAP_MAYMOVE | MREMAP_FIXED, target);
    if (ret == MAP_FAILED) {
        note_fail("mremap 4 KiB of promoted block", strerror(errno));
        munmap(raw, HUGE + HUGE);
        munmap(scratch, HUGE + HUGE);
        return;
    }
    if (ret != target) {
        note_fail("mremap 4 KiB target", "returned addr != requested fixed target");
    } else if (*(unsigned char *)target != 0x71) {
        note_fail("mremap 4 KiB data", "moved page lost its data");
    }
    // Only 4 KiB moved: the block's second page stays mapped and intact (a whole
    // 2 MiB over-move would unmap it).
    if (base[ps] != 0x72) {
        note_fail("mremap 4 KiB over-move", "second page of source block disturbed");
    }

    munmap(raw, HUGE + HUGE);
    munmap(scratch, HUGE + HUGE);
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

    // COW-break split path: fork a shared THP, split it in the child, assert
    // parent/child isolation and refcount-correct data preservation.
    fork_cow_split_isolation();

    // mremap must keep the 4 KiB ABI on a THP-promoted region.
    mremap_4k_of_promoted_block_keeps_4k_abi();

    if (failed == 0) {
        printf("ALL TESTS PASSED\n");
        return 0;
    }
    printf("SOME TESTS FAILED\n");
    return 1;
}
