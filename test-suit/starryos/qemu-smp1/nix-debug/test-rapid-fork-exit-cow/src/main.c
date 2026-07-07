/*
 * test-rapid-fork-exit-cow — NIXPKGS-003 hypothesis:
 * Rapid fork/exit cycles with CoW page writes stress frame refcounting.
 * FrameRefCnt is u8 (max 255); rapid cycling could trigger edge cases
 * in the FRAME_TABLE management.
 *
 * Pattern: fork 20 children sequentially, each writes to shared CoW
 * pages, exits. Parent verifies integrity after all children done.
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

#define PAGE_SIZE   4096
#define PAGE_COUNT  32
#define CHILDREN    20

static unsigned long raw_brk(unsigned long addr)
{
    return syscall(SYS_brk, addr);
}

int main(void)
{
    printf("NIX_DEBUG_RAPID_FORK_BEGIN\n");
    TEST_START("rapid-fork-exit-cow: rapid fork/exit with CoW writes");

    unsigned long orig_brk = raw_brk(0);
    CHECK(orig_brk > 0, "raw_brk(0) valid");
    if (orig_brk == 0) return 1;

    unsigned long new_brk = orig_brk + PAGE_COUNT * PAGE_SIZE;
    CHECK(raw_brk(new_brk) == new_brk, "brk expand succeeds");

    void *mmap_region = mmap(NULL, PAGE_COUNT * PAGE_SIZE,
                              PROT_READ | PROT_WRITE,
                              MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(mmap_region != MAP_FAILED, "mmap succeeds");

    /* Write initial pattern */
    memset((void *)orig_brk, 0xCC, PAGE_COUNT * PAGE_SIZE);
    memset(mmap_region, 0xCC, PAGE_COUNT * PAGE_SIZE);

    int total_ok = 1;

    for (int child_nr = 0; child_nr < CHILDREN; child_nr++) {
        pid_t child = fork();
        CHECK(child >= 0, "fork succeeds");
        if (child < 0) break;

        if (child == 0) {
            /* Each child writes to a different subset of pages */
            int start_page = child_nr % PAGE_COUNT;
            int pages_to_write = 4;
            for (int p = start_page; p < start_page + pages_to_write && p < PAGE_COUNT; p++) {
                memset((void *)(orig_brk + p * PAGE_SIZE), (unsigned char)(0xD0 + child_nr), PAGE_SIZE);
                memset((char *)mmap_region + p * PAGE_SIZE, (unsigned char)(0xD0 + child_nr), PAGE_SIZE);
            }
            _exit(0);
        }

        int status = 0;
        waitpid(child, &status, 0);
        if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
            printf("  DIAG: child %d abnormal exit\n", child_nr);
            total_ok = 0;
        }

        /* Verify parent pages are intact after each child */
        int corrupt = 0;
        for (int p = 0; p < PAGE_COUNT; p++) {
            if (((unsigned char *)orig_brk)[p * PAGE_SIZE] != 0xCC) {
                printf("  DIAG: child=%d brk page %d corrupted: 0x%x\n",
                       child_nr, p, ((unsigned char *)orig_brk)[p * PAGE_SIZE]);
                corrupt = 1;
            }
            if (((unsigned char *)mmap_region)[p * PAGE_SIZE] != 0xCC) {
                printf("  DIAG: child=%d mmap page %d corrupted: 0x%x\n",
                       child_nr, p, ((unsigned char *)mmap_region)[p * PAGE_SIZE]);
                corrupt = 1;
            }
        }
        if (corrupt) total_ok = 0;
    }

    CHECK(total_ok, "all parent pages intact after 20 fork/exit cycles");

    raw_brk(orig_brk);
    munmap(mmap_region, PAGE_COUNT * PAGE_SIZE);

    printf("NIX_DEBUG_RAPID_FORK_PASSED\n");
    return 0;
}
