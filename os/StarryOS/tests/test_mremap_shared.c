#include <sys/mman.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>

int main() {
    int passed = 1;

    // Test: mremap a MAP_SHARED|MAP_ANONYMOUS mapping should preserve shared semantics
    void *shared = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                        MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (shared == MAP_FAILED) {
        printf("SKIP: mmap MAP_SHARED|MAP_ANONYMOUS failed: %s\n", strerror(errno));
        printf("\nTest skipped\n");
        return 0;
    }

    memcpy(shared, "HELLO", 5);

    void *remapped = mremap(shared, 4096, 8192, MREMAP_MAYMOVE);
    if (remapped == MAP_FAILED) {
        printf("FAIL: mremap of MAP_SHARED mapping failed: %s\n", strerror(errno));
        passed = 0;
    } else {
        if (memcmp(remapped, "HELLO", 5) == 0) {
            printf("PASS: mremap preserved data in MAP_SHARED mapping\n");
        } else {
            printf("FAIL: mremap did not preserve data in MAP_SHARED mapping\n");
            passed = 0;
        }
        munmap(remapped, 8192);
    }

    // Test: mremap a MAP_PRIVATE mapping should still work
    void *priv = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (priv == MAP_FAILED) {
        printf("FAIL: mmap MAP_PRIVATE|MAP_ANONYMOUS failed: %s\n", strerror(errno));
        passed = 0;
    } else {
        memcpy(priv, "WORLD", 5);
        void *remapped2 = mremap(priv, 4096, 8192, MREMAP_MAYMOVE);
        if (remapped2 == MAP_FAILED) {
            printf("FAIL: mremap of MAP_PRIVATE mapping failed: %s\n", strerror(errno));
            passed = 0;
        } else {
            if (memcmp(remapped2, "WORLD", 5) == 0) {
                printf("PASS: mremap preserved data in MAP_PRIVATE mapping\n");
            } else {
                printf("FAIL: mremap did not preserve data in MAP_PRIVATE mapping\n");
                passed = 0;
            }
            munmap(remapped2, 8192);
        }
    }

    if (passed) {
        printf("\nAll tests PASSED\n");
        return 0;
    } else {
        printf("\nSome tests FAILED\n");
        return 1;
    }
}
