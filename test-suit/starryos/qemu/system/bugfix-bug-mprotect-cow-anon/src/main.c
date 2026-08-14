/*
 * bug-mprotect-cow-anon: mprotect(+W) on a COW-shared *anonymous* private page
 * must not hand out a writable PTE onto the shared frame. It has to keep the
 * PTE read-only so the first store faults into the COW break path, otherwise a
 * child that re-asserts write permission scribbles straight through the frame
 * the parent still shares — cross-process corruption.
 *
 * Regression for pte_flags_for_protect, which previously stripped WRITE only for
 * file-backed mappings and left anonymous COW pages writable-in-place.
 */
#define _GNU_SOURCE

#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

enum {
    TEST_PAGE_SIZE = 4096,
    PARENT_FILL = 0xAA,
    CHILD_FILL = 0xBB,
};

int main(void)
{
    printf("=== bug-mprotect-cow-anon ===\n");
    printf("Expected: a child's mprotect(+W)+store on a COW-shared anon page\n");
    printf("          breaks COW privately and never corrupts the parent.\n\n");

    unsigned char *page = mmap(NULL, TEST_PAGE_SIZE, PROT_READ | PROT_WRITE,
                               MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (page == MAP_FAILED) {
        printf("FAIL: mmap: %s\n", strerror(errno));
        printf("TEST FAILED\n");
        return 1;
    }

    /* Fault in a private frame and lay down the parent's sentinel. */
    memset(page, PARENT_FILL, TEST_PAGE_SIZE);

    pid_t pid = fork();
    if (pid < 0) {
        printf("FAIL: fork: %s\n", strerror(errno));
        printf("TEST FAILED\n");
        return 1;
    }

    if (pid == 0) {
        /*
         * The page is now COW-shared read-only between parent and child. Drop
         * to PROT_READ then re-assert PROT_READ|PROT_WRITE so the +W protect
         * path runs on the shared page, then store. A correct kernel keeps the
         * PTE read-only until this store COW-breaks into a private frame.
         */
        if (mprotect(page, TEST_PAGE_SIZE, PROT_READ) != 0) {
            _exit(2);
        }
        if (mprotect(page, TEST_PAGE_SIZE, PROT_READ | PROT_WRITE) != 0) {
            _exit(3);
        }
        memset(page, CHILD_FILL, TEST_PAGE_SIZE);
        /* The child must observe its own private store. */
        _exit(page[0] == CHILD_FILL ? 0 : 4);
    }

    int status = 0;
    if (waitpid(pid, &status, 0) != pid) {
        printf("FAIL: waitpid: %s\n", strerror(errno));
        printf("TEST FAILED\n");
        return 1;
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        printf("FAIL: child exit status %d (WIFEXITED=%d, code=%d)\n", status,
               WIFEXITED(status), WIFEXITED(status) ? WEXITSTATUS(status) : -1);
        printf("TEST FAILED\n");
        return 1;
    }

    /* The parent's copy must be untouched by the child's mprotect(+W)+store. */
    for (size_t i = 0; i < TEST_PAGE_SIZE; i++) {
        if (page[i] != PARENT_FILL) {
            printf("FAIL: parent page corrupted at offset %zu: got 0x%02x, "
                   "want 0x%02x\n",
                   i, page[i], PARENT_FILL);
            printf("      child's mprotect(+W) wrote through the shared COW "
                   "frame\n");
            printf("TEST FAILED\n");
            return 1;
        }
    }
    printf("PASS: parent stayed isolated after child mprotect(+W) on COW page\n");

    /*
     * Exclusive (refcount==1) path: mprotect round-trip on a page that is not
     * COW-shared must still leave it writable (the re-enable path).
     */
    unsigned char *excl = mmap(NULL, TEST_PAGE_SIZE, PROT_READ | PROT_WRITE,
                               MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (excl == MAP_FAILED) {
        printf("FAIL: mmap(exclusive): %s\n", strerror(errno));
        printf("TEST FAILED\n");
        return 1;
    }
    excl[0] = 0x11;
    if (mprotect(excl, TEST_PAGE_SIZE, PROT_READ) != 0 ||
        mprotect(excl, TEST_PAGE_SIZE, PROT_READ | PROT_WRITE) != 0) {
        printf("FAIL: mprotect round-trip on exclusive page: %s\n",
               strerror(errno));
        printf("TEST FAILED\n");
        return 1;
    }
    excl[0] = 0x22;
    if (excl[0] != 0x22) {
        printf("FAIL: exclusive page not writable after mprotect round-trip\n");
        printf("TEST FAILED\n");
        return 1;
    }
    printf("PASS: exclusive page still writable after mprotect round-trip\n");

    printf("TEST PASSED\n");
    return 0;
}
