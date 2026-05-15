/* kcov-spec §3,§4: mmap size and buffer layout */
#include "test_framework.h"
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

/* Lowest virtual address in kernel space, architecture-specific. */
#if defined(__x86_64__) || defined(__amd64__)
#define KERNEL_PC_MIN 0xffff800000000000ULL
#elif defined(__aarch64__)
#define KERNEL_PC_MIN 0xffff000000000000ULL
#elif defined(__riscv) && __riscv_xlen == 64
#define KERNEL_PC_MIN 0xffffffc000000000ULL
#elif defined(__loongarch64)
#define KERNEL_PC_MIN 0x9000000000000000ULL
#else
#define KERNEL_PC_MIN 0x8000000000000000ULL
#endif

#define KCOV_INIT_TRACE _IOR('c', 1, unsigned long)
#define KCOV_ENABLE _IO('c', 100)
#define KCOV_DISABLE _IO('c', 101)
#define KCOV_TRACE_PC 0

static void burst(int n) {
    for (volatile int i = 0; i < n; i++) {
        getpid();
        getuid();
        getppid();
    }
}

int main(void) {
    TEST_START("KCOV §3/§4: mmap size, buffer layout");

    int fd = open("/dev/kcov", O_RDWR);
    CHECK(fd >= 0, "open");

    /* §3: mmap with correct size succeeds */
    CHECK_RET(ioctl(fd, KCOV_INIT_TRACE, 256), 0, "INIT_TRACE size=256");
    size_t sz = 256 * sizeof(uint64_t);
    uint64_t *buf = mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    CHECK_PTR(buf, 1, "mmap with correct size");
    if (buf == MAP_FAILED) {
        close(fd);
        TEST_DONE();
    }

    /* §4: buffer layout — cover[0]=count, starts at 0 */
    CHECK(buf[0] == 0, "cover[0] (count) starts at 0");

    CHECK_RET(ioctl(fd, KCOV_ENABLE, KCOV_TRACE_PC), 0, "ENABLE");
    burst(500);
    CHECK_RET(ioctl(fd, KCOV_DISABLE, 0), 0, "DISABLE");

    /* §4: count reflects number of PCs collected */
    uint64_t n = buf[0];
    CHECK(n >= 1, "count >= 1 after tracing");

    /* §4: PCs start at cover[1], non-zero */
    int ok = 1;
    for (uint64_t i = 0; i < n; i++) {
        if (buf[1 + i] == 0)
            ok = 0;
        printf("  TRACE: cover[%lu]=0x%016lx\n", 1 + i, buf[1 + i]);
    }
    CHECK(ok, "cover[1..n] are non-zero");

    ok = 1;
    for (uint64_t i = 0; i < n; i++) {
        if (!(buf[i + 1] >= KERNEL_PC_MIN))
            ok = 0;
    }
    CHECK(ok, "PCs in kernel range");

    /* §3: buffer writable after disable (Linux spec: buffer still accessible)
     */
    buf[1] = 0xCAFEBABEDEADBEEFULL;
    CHECK(buf[1] == 0xCAFEBABEDEADBEEFULL, "buffer writable after DISABLE");

    munmap(buf, sz);
    close(fd);

    /* Upper-bound: medium buffer, light burst — records but won't fill */

    // If this test fails, try set the buffer larger as the failure might be
    // caused by too many edges instrumented.
    {
        int fd2 = open("/dev/kcov", O_RDWR);
        CHECK(fd2 >= 0, "open for upper-bound test");
        CHECK_RET(ioctl(fd2, KCOV_INIT_TRACE, 10240), 0,
                  "INIT_TRACE size=10240");
        size_t sz2 = 10240 * sizeof(uint64_t);
        uint64_t *b2 =
            mmap(NULL, sz2, PROT_READ | PROT_WRITE, MAP_SHARED, fd2, 0);
        CHECK_PTR(b2, 1, "mmap upper-bound test");
        if (b2 != MAP_FAILED) {
            CHECK_RET(ioctl(fd2, KCOV_ENABLE, KCOV_TRACE_PC), 0, "ENABLE");
            burst(5);
            CHECK_RET(ioctl(fd2, KCOV_DISABLE, 0), 0, "DISABLE");
            uint64_t n2 = b2[0];
            printf("  TRACE: upper-bound count=%lu (cap=1024)\n", n2);
            CHECK(n2 > 0, "upper-bound: count > 0");
            CHECK(n2 < 10200, "upper-bound: count < upper-bound (not full)");
            munmap(b2, sz2);
        }
        close(fd2);
    }

    TEST_DONE();
}
