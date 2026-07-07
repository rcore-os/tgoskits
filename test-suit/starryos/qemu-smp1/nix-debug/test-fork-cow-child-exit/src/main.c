/*
 * test-fork-cow-child-exit — NIXPKGS-003 hypothesis:
 * Child process writes to CoW pages then exits; do_exit → AddrSpace::Drop
 * may incorrectly modify parent's frame refcounts, causing parent heap
 * corruption.
 *
 * Pattern: fork → child writes brk+mmap pages → child exits → parent verifies
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

#define PATTERN_PARENT 0xA1
#define PATTERN_CHILD  0xB2
#define PAGE_COUNT     64
#define PAGE_SIZE      4096

static unsigned long raw_brk(unsigned long addr)
{
    return syscall(SYS_brk, addr);
}

int main(void)
{
    printf("NIX_DEBUG_FORK_COW_BEGIN\n");
    TEST_START("fork-cow-child-exit: child writes CoW pages then exits");

    unsigned long orig_brk = raw_brk(0);
    CHECK(orig_brk > 0, "raw_brk(0) valid");
    if (orig_brk == 0) return 1;

    /* Expand brk */
    unsigned long new_brk = orig_brk + PAGE_COUNT * PAGE_SIZE;
    unsigned long br = raw_brk(new_brk);
    CHECK(br == new_brk, "brk expand succeeds");
    if (br != new_brk) return 1;

    /* Allocate MAP_PRIVATE region (simulates glibc malloc arena) */
    void *mmap_region = mmap(NULL, PAGE_COUNT * PAGE_SIZE,
                              PROT_READ | PROT_WRITE,
                              MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(mmap_region != MAP_FAILED, "mmap MAP_PRIVATE succeeds");

    /* Write parent pattern to both regions */
    memset((void *)orig_brk, PATTERN_PARENT, PAGE_COUNT * PAGE_SIZE);
    memset(mmap_region, PATTERN_PARENT, PAGE_COUNT * PAGE_SIZE);

    pid_t child = fork();
    CHECK(child >= 0, "fork succeeds");
    if (child < 0) {
        raw_brk(orig_brk);
        munmap(mmap_region, PAGE_COUNT * PAGE_SIZE);
        return 1;
    }

    if (child == 0) {
        /* Child: write different pattern to trigger CoW copies */
        memset((void *)orig_brk, PATTERN_CHILD, PAGE_COUNT * PAGE_SIZE);
        memset(mmap_region, PATTERN_CHILD, PAGE_COUNT * PAGE_SIZE);

        /* Verify child sees its own pattern */
        int ok = 1;
        for (int i = 0; i < PAGE_COUNT; i++) {
            if (((unsigned char *)orig_brk)[i * PAGE_SIZE] != PATTERN_CHILD) ok = 0;
            if (((unsigned char *)mmap_region)[i * PAGE_SIZE] != PATTERN_CHILD) ok = 0;
        }
        printf("CHILD_PATTERN_CHECK: ok=%d\n", ok);
        _exit(ok ? 0 : 1);
    }

    /* Parent: wait for child */
    int status = 0;
    pid_t waited = waitpid(child, &status, 0);
    CHECK(waited == child, "waitpid collects child");

    if (WIFEXITED(status)) {
        int code = WEXITSTATUS(status);
        CHECK(code == 0, "child exited 0 (pattern check passed)");
    } else if (WIFSIGNALED(status)) {
        printf("  DIAG: child killed by signal %d\n", WTERMSIG(status));
        CHECK(0, "child not killed by signal");
    }

    /* Parent verifies its pages still have PARENT pattern (CoW isolated) */
    int brk_ok = 1, mmap_ok = 1;
    for (int i = 0; i < PAGE_COUNT; i++) {
        if (((unsigned char *)orig_brk)[i * PAGE_SIZE] != PATTERN_PARENT) {
            printf("  DIAG: brk page %d corrupted: expected 0x%x got 0x%x\n",
                   i, PATTERN_PARENT, ((unsigned char *)orig_brk)[i * PAGE_SIZE]);
            brk_ok = 0;
        }
        if (((unsigned char *)mmap_region)[i * PAGE_SIZE] != PATTERN_PARENT) {
            printf("  DIAG: mmap page %d corrupted: expected 0x%x got 0x%x\n",
                   i, PATTERN_PARENT, ((unsigned char *)mmap_region)[i * PAGE_SIZE]);
            mmap_ok = 0;
        }
    }
    CHECK(brk_ok, "parent brk pages intact after child exit");
    CHECK(mmap_ok, "parent mmap pages intact after child exit");

    /* Cleanup */
    raw_brk(orig_brk);
    munmap(mmap_region, PAGE_COUNT * PAGE_SIZE);

    printf("NIX_DEBUG_FORK_COW_PASSED\n");
    return 0;
}
