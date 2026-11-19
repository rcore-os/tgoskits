#define _GNU_SOURCE

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SYS_riscv_hwprobe
#define SYS_riscv_hwprobe 258
#endif

#define RISCV_HWPROBE_KEY_BASE_BEHAVIOR 3
#define RISCV_HWPROBE_BASE_BEHAVIOR_IMA (1ULL << 0)
#define RISCV_HWPROBE_KEY_IMA_EXT_0 4
#define RISCV_HWPROBE_IMA_FD (1ULL << 0)
#define RISCV_HWPROBE_IMA_C (1ULL << 1)

struct riscv_hwprobe {
    int64_t key;
    uint64_t value;
};

static int test_passed = 0;
static int test_failed = 0;

#define CHECK(cond, msg) \
    do { \
        if (!(cond)) { \
            printf("  [FAIL] %s\n", (msg)); \
            test_failed++; \
        } else { \
            printf("  [OK] %s\n", (msg)); \
            test_passed++; \
        } \
    } while (0)

#define CHECK_ERRNO(call, want, msg) \
    do { \
        errno = 0; \
        long _ret = (call); \
        CHECK(_ret == -1 && errno == (want), (msg)); \
    } while (0)

int main(void)
{
    printf("[TEST] riscv_hwprobe conservative ABI\n");

    struct riscv_hwprobe pairs[] = {
        { RISCV_HWPROBE_KEY_BASE_BEHAVIOR, 0 },
        { RISCV_HWPROBE_KEY_IMA_EXT_0, 0 },
        { 999999, 1234 },
    };

    errno = 0;
    long ret = syscall(SYS_riscv_hwprobe, pairs, 3, 0, NULL, 0);
    CHECK(ret == 0, "riscv_hwprobe returns success for default CPU set");
    CHECK(errno == 0, "errno unchanged on success");
    CHECK(pairs[0].key == RISCV_HWPROBE_KEY_BASE_BEHAVIOR, "base behavior key preserved");
    CHECK((pairs[0].value & RISCV_HWPROBE_BASE_BEHAVIOR_IMA) != 0,
          "base IMA behavior reported");
    CHECK(pairs[1].key == RISCV_HWPROBE_KEY_IMA_EXT_0, "IMA extension key preserved");
    CHECK((pairs[1].value & RISCV_HWPROBE_IMA_FD) != 0, "F/D extension reported");
    CHECK((pairs[1].value & RISCV_HWPROBE_IMA_C) != 0, "C extension reported");
    CHECK(pairs[2].key == -1 && pairs[2].value == 0, "unknown key is marked unsupported");

    ret = syscall(SYS_riscv_hwprobe, NULL, 0, 0, NULL, 0);
    CHECK(ret == 0, "zero pair count accepts NULL pairs");

    CHECK_ERRNO(syscall(SYS_riscv_hwprobe, pairs, 1, 0, NULL, 1), EINVAL,
                "unsupported flags return EINVAL");
    CHECK_ERRNO(syscall(SYS_riscv_hwprobe, pairs, 1, 1, NULL, 0), EINVAL,
                "explicit CPU count without CPU set returns EINVAL");
    unsigned long cpuset = 1;
    CHECK_ERRNO(syscall(SYS_riscv_hwprobe, pairs, 1, 1, &cpuset, 0), EINVAL,
                "explicit CPU set returns EINVAL");
    CHECK_ERRNO(syscall(SYS_riscv_hwprobe, (void *)1, 1, 0, NULL, 0), EFAULT,
                "bad pairs pointer returns EFAULT");

    const size_t page_size = (size_t)sysconf(_SC_PAGESIZE);
    unsigned char *region = mmap(NULL, 2 * page_size, PROT_READ | PROT_WRITE,
                                 MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(region != MAP_FAILED, "allocate pair boundary fixture");
    if (region != MAP_FAILED) {
        struct riscv_hwprobe *first =
            (struct riscv_hwprobe *)(region + page_size - sizeof(*first));
        first->key = RISCV_HWPROBE_KEY_BASE_BEHAVIOR;
        first->value = UINT64_MAX;
        int protected = mprotect(region + page_size, page_size, PROT_NONE);
        CHECK(protected == 0, "protect the following pair");
        if (protected == 0) {
            CHECK_ERRNO(syscall(SYS_riscv_hwprobe, first, 2, 0, NULL, 0), EFAULT,
                        "later pair reports its copy fault");
            CHECK(first->value != UINT64_MAX &&
                      (first->value & RISCV_HWPROBE_BASE_BEHAVIOR_IMA) != 0,
                  "completed pair remains visible before a later copy fault");
        }
        munmap(region, 2 * page_size);
    }
    if (test_failed != 0) {
        printf("TEST FAILED: pair publication regression\n");
        return EXIT_FAILURE;
    }
    CHECK_ERRNO(syscall(SYS_riscv_hwprobe, (void *)1, (size_t)1 << 32,
                        0, NULL, 0), EFAULT,
                "large pair count requires no array-sized scratch allocation");

    printf("\n=== result: %d passed, %d failed ===\n", test_passed, test_failed);
    if (test_failed == 0) {
        printf("TEST PASSED\n");
    }
    return test_failed == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}
