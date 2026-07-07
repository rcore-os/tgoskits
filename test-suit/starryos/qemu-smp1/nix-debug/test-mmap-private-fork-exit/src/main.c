/*
 * test-mmap-private-fork-exit — NIXPKGS-003 hypothesis:
 * MAP_PRIVATE CoW isolation breaks after child exit. glibc uses
 * mmap(MAP_PRIVATE|MAP_ANONYMOUS) for malloc arenas; if CoW isolation
 * fails, child writes leak into parent's heap pages.
 *
 * Pattern: mmap → fork → child writes → child exits → parent verifies
 * Multiple iterations to stress frame refcounting.
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

#define PAGE_SIZE   4096
#define REGION_SIZE (64 * PAGE_SIZE)
#define ITERATIONS  5

int main(void)
{
    printf("NIX_DEBUG_MMAP_FORK_BEGIN\n");
    TEST_START("mmap-private-fork-exit: MAP_PRIVATE CoW isolation after child exit");

    for (int iter = 0; iter < ITERATIONS; iter++) {
        void *region = mmap(NULL, REGION_SIZE, PROT_READ | PROT_WRITE,
                            MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(region != MAP_FAILED, "mmap succeeds");

        /* Write parent pattern */
        memset(region, (unsigned char)(0xA0 + iter), REGION_SIZE);

        pid_t child = fork();
        CHECK(child >= 0, "fork succeeds");
        if (child < 0) { munmap(region, REGION_SIZE); return 1; }

        if (child == 0) {
            /* Child writes every page */
            memset(region, (unsigned char)(0xB0 + iter), REGION_SIZE);

            /* Verify child pattern */
            int ok = 1;
            for (int p = 0; p < REGION_SIZE / PAGE_SIZE; p++) {
                if (((unsigned char *)region)[p * PAGE_SIZE] != (unsigned char)(0xB0 + iter))
                    ok = 0;
            }
            _exit(ok ? 0 : 1);
        }

        int status = 0;
        waitpid(child, &status, 0);

        if (WIFEXITED(status) && WEXITSTATUS(status) != 0) {
            printf("  DIAG: iter=%d child exit=%d\n", iter, WEXITSTATUS(status));
        }

        /* Parent verifies: all pages should still have parent pattern */
        int corrupt = 0;
        for (int p = 0; p < REGION_SIZE / PAGE_SIZE; p++) {
            unsigned char val = ((unsigned char *)region)[p * PAGE_SIZE];
            if (val != (unsigned char)(0xA0 + iter)) {
                printf("  DIAG: iter=%d page=%d corrupted: expected 0x%x got 0x%x\n",
                       iter, p, 0xA0 + iter, val);
                corrupt = 1;
                break;
            }
        }
        CHECK(!corrupt, "MAP_PRIVATE region intact after child exit");

        munmap(region, REGION_SIZE);
    }

    printf("NIX_DEBUG_MMAP_FORK_PASSED\n");
    return 0;
}
