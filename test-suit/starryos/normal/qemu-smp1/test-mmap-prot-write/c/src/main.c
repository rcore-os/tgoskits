/*
 * test-mmap-prot-write
 *
 * Verifies that mmap(PROT_WRITE | MAP_ANON) returns a region that is
 * readable as well as writable. RISC-V's privileged spec reserves the
 * (R=0, W=1) PTE encoding, so a write-only mapping would either fault
 * on the first access or be unusable; Linux always promotes PROT_WRITE
 * to PROT_READ | PROT_WRITE for this reason. weston's drm-pixman
 * shadow framebuffer path mmaps dumb buffers with PROT_WRITE alone.
 */

#include "test_framework.h"
#include <sys/mman.h>
#include <unistd.h>
#include <string.h>

int main(void)
{
    TEST_START("mmap PROT_WRITE auto-promotes to R+W (Linux semantics)");

    size_t len = 4096;
    void *p = mmap(NULL, len, PROT_WRITE, MAP_ANON | MAP_PRIVATE, -1, 0);
    CHECK(p != MAP_FAILED, "mmap PROT_WRITE returns valid pointer");
    if (p == MAP_FAILED) {
        TEST_DONE();
    }

    /* Write a sentinel pattern. */
    unsigned char *buf = (unsigned char *)p;
    for (size_t i = 0; i < 32; i++) {
        buf[i] = (unsigned char)(0xa5 ^ i);
    }

    /* Read it back. On a kernel that left the PTE write-only this is
     * the access that page-faults. */
    int ok = 1;
    for (size_t i = 0; i < 32; i++) {
        if (buf[i] != (unsigned char)(0xa5 ^ i)) {
            ok = 0;
            break;
        }
    }
    CHECK(ok, "PROT_WRITE region is also readable");

    CHECK_RET(munmap(p, len), 0, "munmap");

    TEST_DONE();
}
