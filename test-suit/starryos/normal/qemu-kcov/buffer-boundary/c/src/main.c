/*
 * test-kcov-buffer-boundary — regression test for buffer off-by-one.
 *
 * The kcov buffer layout is: [count: u64 | pc[0]: u64 | ... | pc[N-1]: u64]
 * where the total number of u64 entries is the `cover_size` arg passed to
 * KCOV_INIT_TRACE (matching Linux's kcov->size).
 *
 * Maximum number of PC entries = cover_size - 1.
 *
 * This test uses cover_size=512 (exactly one page of u64 entries = 4096 bytes)
 * to force the buffer onto a single physical page.  With the old off-by-one
 * bug the kernel allocated (1+cover_size) entries, placing the last PC slot
 * at byte offset cover_size*8 — the first byte beyond userspace's mmmap.
 * The fix allocates exactly cover_size entries, so the last PC fits.
 *
 * The test fills the buffer completely, then verifies that every PC slot
 * (1..cover_size-1) contains a valid kernel-space address.
 */

#define _GNU_SOURCE
#include "test_framework.h"
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

/* ---- KCOV ioctl constants ---- */

#define KCOV_INIT_TRACE _IOR('c', 1, unsigned long)
#define KCOV_ENABLE     _IO('c', 100)
#define KCOV_DISABLE    _IO('c', 101)
#define KCOV_TRACE_PC   0

/* Exactly one page of u64 entries = 4096 bytes. */
#define COVER_SIZE     512
#define MMAP_BYTES     (COVER_SIZE * sizeof(uint64_t))

/* Architecture kernel-space PC floor. */
#if defined(__x86_64__) || defined(__amd64__)
#define KERNEL_PC_MIN 0xffff800000000000ULL
#elif defined(__aarch64__)
#define KERNEL_PC_MIN 0xffff000000000000ULL
#elif defined(__riscv) && __riscv_xlen == 64
#define KERNEL_PC_MIN 0xffffffc000000000ULL
#else
#define KERNEL_PC_MIN 0x8000000000000000ULL
#endif

/* ---- helpers ---- */

/* Tight syscall loop to generate enough distinct PCs to fill the buffer. */
static void heavy_burst(void) {
    for (volatile int i = 0; i < 100000; i++) {
        getpid();
        getuid();
        getppid();
    }
}

/* ---- main ---- */

int main(void) {
    TEST_START("KCOV buffer boundary — no off-by-one");

    int fd = open("/dev/kcov", O_RDWR);
    if (fd < 0) {
        printf("SKIP: /dev/kcov not available (errno=%d)\n", errno);
        return 0;
    }

    CHECK_RET(ioctl(fd, KCOV_INIT_TRACE, COVER_SIZE), 0,
              "INIT_TRACE");

    uint64_t *buf = mmap(NULL, MMAP_BYTES, PROT_READ | PROT_WRITE,
                         MAP_SHARED, fd, 0);
    CHECK(buf != MAP_FAILED, "mmap buffer");

    CHECK_RET(ioctl(fd, KCOV_ENABLE, KCOV_TRACE_PC), 0, "ENABLE");

    /* Generate enough coverage to fill all PC slots. */
    heavy_burst();

    uint64_t count = buf[0];
    printf("  INFO: count=%lu (expecting %d)\n", count, COVER_SIZE - 1);

    /* The count should be cover_size-1 = 511 (all PC slots filled).
     * If the old off-by-one were present the kernel would allocate
     * (1+cover_size) entries, and the last PC would go into an
     * unmapped slot — but the count would still show cover_size. */
    CHECK(count == COVER_SIZE - 1,
          "buffer completely filled");

    /* Verify every PC slot is a valid kernel address.
     * The critical regression check: buf[COVER_SIZE-1] must be
     * accessible (with the old off-by-one it lived beyond the mmmap). */
    int all_valid = 1;
    uint64_t first_pc = buf[1];
    for (int i = 1; i < COVER_SIZE; i++) {
        uint64_t pc = buf[i];
        if (pc < KERNEL_PC_MIN) {
            printf("  FAIL: buf[%d] = 0x%lx (not a kernel address)\n",
                   i, pc);
            all_valid = 0;
        }
    }
    CHECK(all_valid, "all PC slots valid");

    /* Explicit check on the very last slot (the off-by-one boundary). */
    uint64_t last_pc = buf[COVER_SIZE - 1];
    CHECK(last_pc >= KERNEL_PC_MIN,
          "last PC slot accessible and valid");

    /* Verify at least some diversity in collected PCs. */
    int diverse = 0;
    for (int i = 2; i < COVER_SIZE && i <= 20; i++) {
        if (buf[i] != first_pc) {
            diverse = 1;
            break;
        }
    }
    CHECK(diverse, "multiple distinct PCs observed");

    ioctl(fd, KCOV_DISABLE, 0);
    munmap(buf, MMAP_BYTES);
    close(fd);

    TEST_DONE();
}
