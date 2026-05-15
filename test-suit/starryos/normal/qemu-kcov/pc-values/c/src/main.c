/* kcov-spec §7: PC values are kernel instruction pointers */
#include "test_framework.h"
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
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
        struct stat st;
        stat("/", &st);
        open("/dev/null", O_RDONLY);
    }
}

int main(void) {
    TEST_START("KCOV §7: PC values — kernel instruction pointers");

    int fd = open("/dev/kcov", O_RDWR);
    CHECK(fd >= 0, "open");
    CHECK_RET(ioctl(fd, KCOV_INIT_TRACE, 256), 0, "INIT_TRACE");
    size_t sz = 256 * sizeof(uint64_t);
    uint64_t *buf = mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    CHECK_PTR(buf, 1, "mmap");
    if (buf == MAP_FAILED) {
        close(fd);
        TEST_DONE();
    }

    CHECK_RET(ioctl(fd, KCOV_ENABLE, KCOV_TRACE_PC), 0, "ENABLE");
    /* Exercise diverse kernel paths */
    burst(500);
    int tmp = open("/tmp", O_RDONLY | O_DIRECTORY);
    if (tmp >= 0)
        close(tmp);
    char cwd[128];
    getcwd(cwd, sizeof(cwd));
    struct stat st;
    stat("/dev", &st);
    CHECK_RET(ioctl(fd, KCOV_DISABLE, 0), 0, "DISABLE");

    uint64_t n = buf[0];
    CHECK(n >= 1, "recorded ≥ 1 PC");

    /* Doc: PCs are kernel instruction pointers, used with addr2line.
     * All should be in kernel address range, with diversity. */
    int in_range = 1, diverse = 0;
    uint64_t first = buf[1];
    for (uint64_t i = 1; i <= n && i <= 20; i++) {
        if (buf[i] < KERNEL_PC_MIN)
            in_range = 0;
        if (buf[i] != first)
            diverse = 1;
    }
    CHECK(in_range, "all PCs in kernel address range");
    CHECK(diverse, "PCs show diversity (multiple kernel locations)");

    munmap(buf, sz);
    close(fd);
    TEST_DONE();
}
