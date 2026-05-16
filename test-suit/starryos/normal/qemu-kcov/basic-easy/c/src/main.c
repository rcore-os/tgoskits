/*
 * test-kcov.c — KCOV device and SharedPages mmap test
 *
 * Tests the /dev/kcov character device, its ioctl interface, and the
 * shared memory buffer exposed via mmap. Exercises:
 *
 *   1. /dev/kcov exists and is openable
 *   2. KCOV_INIT_TRACE allocates a buffer
 *   3. mmap on the kcov fd returns a writable shared mapping
 *   4. KCOV_ENABLE / KCOV_DISABLE control tracing state
 *   5. The mmap'd buffer records coverage data (non-zero PCs)
 *
 * The test requires the kernel to be built with --features kcov.
 * When /dev/kcov is absent the test reports a skip (exit 0).
 */

#define _GNU_SOURCE
#include "test_framework.h"
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

/* ---- KCOV ioctl constants (matching Linux uapi, not in musl headers) ---- */

#define KCOV_INIT_TRACE _IOR('c', 1, unsigned long)
#define KCOV_ENABLE _IO('c', 100)
#define KCOV_DISABLE _IO('c', 101)

#define KCOV_TRACE_PC 0
#define KCOV_TRACE_CMP 1

/* ---- Test ---- */

int main(void) {
    TEST_START("KCOV /dev/kcov");

    /* Open the kcov device */
    int fd = open("/dev/kcov", O_RDWR);
    if (fd < 0) {
        printf("SKIP: /dev/kcov not available (errno=%d: %s)\n", errno,
               strerror(errno));
        printf("      The kernel must be built with --features kcov\n");
        TEST_DONE(); /* DONE: 0 pass, 0 fail → success for skip */
    }

    /* KCOV_INIT_TRACE: allocate coverage buffer (256 entries) */
    unsigned long cover_size = 256;
    CHECK_RET(ioctl(fd, KCOV_INIT_TRACE, cover_size), 0,
              "KCOV_INIT_TRACE (256 entries)");

    /* mmap the kcov fd: must succeed, return writable memory */
    /*
     * Buffer layout: [count: u64 | pc[0]: u64 | ... | pc[N-1]: u64]
     * Total entries = cover_size (matching Linux: cover_size includes the count word).
     * Available PC slots = cover_size - 1.
     * mmap size = cover_size * sizeof(uint64_t).
     */
    size_t buf_size = cover_size * sizeof(uint64_t);
    uint64_t *buf =
        mmap(NULL, buf_size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    CHECK(buf != MAP_FAILED, "mmap(MAP_SHARED) of kcov fd succeeds");

    /* Verify the mmap'd buffer is accessible and count starts at 0 */
    if (buf != MAP_FAILED) {
        CHECK(buf[0] == 0, "initial coverage count is 0");
    }

    /* KCOV_ENABLE: start tracing (PC mode) */
    CHECK_RET(ioctl(fd, KCOV_ENABLE, KCOV_TRACE_PC), 0,
              "KCOV_ENABLE (KCOV_TRACE_PC)");

    /* Exercise some syscalls to generate coverage */
    {
        int tmp = open("/tmp", O_RDONLY | O_DIRECTORY);
        if (tmp >= 0)
            close(tmp);
        getpid();
        getuid();
        struct stat st;
        stat("/dev", &st);
        char buf_small[64];
        getcwd(buf_small, sizeof(buf_small));
        /* stress the syscall path a bit */
        for (volatile int i = 0; i < 10; i++) {
            getpid();
        }
    }

    /* KCOV_DISABLE: stop tracing */
    CHECK_RET(ioctl(fd, KCOV_DISABLE, 0), 0, "KCOV_DISABLE");

    /* Verify the buffer recorded some coverage (count > 0) */
    if (buf != MAP_FAILED) {
        /*
         * Whether we get coverage depends on compiler instrumentation.
         * Without -fsanitize-coverage the buffer will be empty.
         *
         * The test still passes either way — we verify the mechanism
         * works (mmap, ioctls succeed) rather than requiring actual
         * coverage data unless the kernel was instrumented.
         */
        uint64_t count = buf[0];
        printf("  INFO: recorded %lu coverage entries\n", count);
        for (uint64_t i = 0; i <= count; i++) {
            printf("  TRACE: buf[%lu]=0x%lx\n", i, buf[i]);
        }

        CHECK(count > 0, "coverage count>=1");
        CHECK(count < cover_size, "coverage count<cover_size");
        buf[1] = 0xDEADBEEFCAFE;
        CHECK(buf[1] == (uint64_t)0xDEADBEEFCAFE,
              "mmap'd buffer writable after KCOV_DISABLE");
    }else{
        exit(1);
    }

    /* Cleanup */
    munmap(buf, buf_size);
    close(fd);

    TEST_DONE();
}
