/* kcov-spec §9: Integrity — close while active, overflow, state machine */
#include "test_framework.h"
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

#define KCOV_INIT_TRACE _IOR('c', 1, unsigned long)
#define KCOV_ENABLE _IO('c', 100)
#define KCOV_DISABLE _IO('c', 101)
#define KCOV_RESET_TRACE _IO('c', 104)
#define KCOV_TRACE_PC 0

static void burst(int n) {
    for (volatile int i = 0; i < n; i++) {
        getpid();
        getuid();
        getppid();
    }
}

int main(void) {
    TEST_START("KCOV §9: close-active, overflow, reopen");

    /* Close while active — must not crash */
    {
        int fd = open("/dev/kcov", O_RDWR);
        CHECK_RET(ioctl(fd, KCOV_INIT_TRACE, 256), 0, "INIT_TRACE");
        size_t sz = 256 * sizeof(uint64_t);
        uint64_t *buf =
            mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
        CHECK_PTR(buf, 1, "mmap");
        CHECK_RET(ioctl(fd, KCOV_ENABLE, KCOV_TRACE_PC), 0, "ENABLE");
        burst(50);
        munmap(buf, sz);
        CHECK_RET(close(fd), 0, "close while active — no crash");
    }

    /* Reopen after close */
    {
        int fd = open("/dev/kcov", O_RDWR);
        CHECK(fd >= 0, "reopen after close");
        CHECK_RET(ioctl(fd, KCOV_INIT_TRACE, 64), 0, "INIT_TRACE on reopened");
        close(fd);
    }

    /* Buffer overflow — count stops at capacity */
    {
        int fd = open("/dev/kcov", O_RDWR);
        CHECK_RET(ioctl(fd, KCOV_INIT_TRACE, 256), 0, "INIT_TRACE");
        size_t sz = 256 * sizeof(uint64_t);
        uint64_t *buf =
            mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
        CHECK_PTR(buf, 1, "mmap");
        CHECK_RET(ioctl(fd, KCOV_ENABLE, KCOV_TRACE_PC), 0, "ENABLE");
        burst(30000);
        CHECK_RET(ioctl(fd, KCOV_DISABLE, 0), 0, "DISABLE");
        CHECK(buf[0] > 0, "count > 0 after overflow");
        CHECK(buf[0] <= 256, "count ≤ capacity (no overflow)");
        printf("  INFO: overflow count=%lu (cap=256)\n", buf[0]);
        munmap(buf, sz);
        close(fd);
    }

    /* DISABLE → ENABLE → DISABLE cycles (state machine) */
    {
        int fd = open("/dev/kcov", O_RDWR);
        CHECK_RET(ioctl(fd, KCOV_INIT_TRACE, 256), 0, "INIT_TRACE");
        size_t sz = 256 * sizeof(uint64_t);
        uint64_t *buf =
            mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
        CHECK_PTR(buf, 1, "mmap");
        // Linux: DISABLE before ENABLE (from INIT) → EINVAL
        CHECK_ERR(ioctl(fd, KCOV_DISABLE, 0), EINVAL, "DISABLE before ENABLE → EINVAL");
        for (int c = 1; c <= 3; c++) {
            CHECK_RET(ioctl(fd, KCOV_ENABLE, KCOV_TRACE_PC), 0, "ENABLE");
            burst(30);
            CHECK_RET(ioctl(fd, KCOV_DISABLE, 0), 0, "DISABLE");
        }
        CHECK(buf[0] >= 3, "accumulated across 3 cycles");
        munmap(buf, sz);
        close(fd);
    }

    /* DISABLE with non-zero arg → EINVAL (Linux KCOV_DISABLE semantics) */
    {
        int fd = open("/dev/kcov", O_RDWR);
        CHECK_RET(ioctl(fd, KCOV_INIT_TRACE, 64), 0, "INIT_TRACE");
        CHECK_RET(ioctl(fd, KCOV_ENABLE, KCOV_TRACE_PC), 0, "ENABLE");
        CHECK_ERR(ioctl(fd, KCOV_DISABLE, 1), EINVAL, "DISABLE with arg=1 → EINVAL");
        // After failed DISABLE, tracing is still active — verify and then disable properly.
        burst(10);
        CHECK_RET(ioctl(fd, KCOV_DISABLE, 0), 0, "DISABLE with arg=0 after failed attempt");
        close(fd);
    }

    /* RESET_TRACE with non-zero arg → EINVAL */
    {
        int fd = open("/dev/kcov", O_RDWR);
        CHECK_RET(ioctl(fd, KCOV_INIT_TRACE, 64), 0, "INIT_TRACE");
        CHECK_RET(ioctl(fd, KCOV_ENABLE, KCOV_TRACE_PC), 0, "ENABLE");
        burst(10);
        CHECK_ERR(ioctl(fd, KCOV_RESET_TRACE, 1), EINVAL, "RESET_TRACE with arg=1 → EINVAL");
        CHECK_RET(ioctl(fd, KCOV_DISABLE, 0), 0, "DISABLE after failed RESET_TRACE");
        close(fd);
    }

    TEST_DONE();
}
