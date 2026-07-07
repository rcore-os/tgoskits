/*
 * test-malloc-fork-stress — NIXPKGS-003 hypothesis:
 * glibc malloc uses brk() for small allocations and mmap() for large ones.
 * If StarryOS brk/mmap state is corrupted during fork/exit, musl's malloc
 * (which uses similar patterns) should also show corruption.
 *
 * Pattern: parent mallocs → fork child → child frees some, mallocs new →
 * child exits → parent frees all → verify no double-free or corruption.
 */

#include "test_framework.h"

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

#define ALLOC_COUNT  64
#define CHILD_FREE   16
#define CHILD_ALLOC  8

int main(void)
{
    printf("NIX_DEBUG_MALLOC_FORK_BEGIN\n");
    TEST_START("malloc-fork-stress: malloc/free across fork/exit");

    void *blocks[ALLOC_COUNT] = {NULL};

    /* Parent: allocate blocks of varying sizes */
    for (int i = 0; i < ALLOC_COUNT; i++) {
        size_t sz = 128 + (i * 97) % 8192;
        blocks[i] = malloc(sz);
        CHECK(blocks[i] != NULL, "parent malloc succeeds");
        if (blocks[i]) memset(blocks[i], (unsigned char)i, sz);
    }

    pid_t child = fork();
    CHECK(child >= 0, "fork succeeds");
    if (child < 0) {
        for (int i = 0; i < ALLOC_COUNT; i++) free(blocks[i]);
        return 1;
    }

    if (child == 0) {
        /* Child: free some blocks, allocate new ones */
        for (int i = 0; i < CHILD_FREE; i++) {
            free(blocks[i]);
            blocks[i] = NULL;
        }

        void *child_blocks[CHILD_ALLOC] = {NULL};
        for (int i = 0; i < CHILD_ALLOC; i++) {
            child_blocks[i] = malloc(512 + i * 64);
            if (child_blocks[i]) memset(child_blocks[i], 0xEE, 512 + i * 64);
        }

        /* Free child's own allocations */
        for (int i = 0; i < CHILD_ALLOC; i++) free(child_blocks[i]);

        /* Free remaining parent blocks */
        for (int i = CHILD_FREE; i < ALLOC_COUNT; i++) free(blocks[i]);

        _exit(0);
    }

    /* Parent: wait for child */
    int status = 0;
    waitpid(child, &status, 0);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "child exits cleanly");

    /* Parent: verify remaining blocks still have correct data */
    int corrupt = 0;
    for (int i = 0; i < ALLOC_COUNT; i++) {
        if (!blocks[i]) continue;
        unsigned char expected = (unsigned char)i;
        if (((unsigned char *)blocks[i])[0] != expected) {
            printf("  DIAG: block %d corrupted: expected 0x%x got 0x%x\n",
                   i, expected, ((unsigned char *)blocks[i])[0]);
            corrupt = 1;
        }
    }
    CHECK(!corrupt, "parent malloc blocks intact after child fork/exit");

    /* Parent: free all blocks (should not double-free or crash) */
    for (int i = 0; i < ALLOC_COUNT; i++) free(blocks[i]);

    /* Allocate again to verify heap is still usable */
    void *post = malloc(4096);
    CHECK(post != NULL, "malloc after fork/exit/free still works");
    if (post) {
        memset(post, 0x99, 4096);
        CHECK(((unsigned char *)post)[0] == 0x99, "post-fork malloc writable");
        free(post);
    }

    printf("NIX_DEBUG_MALLOC_FORK_PASSED\n");
    return 0;
}
