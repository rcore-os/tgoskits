/* kcov-spec §5,§6: ENABLE/DISABLE, per-task, re-enable */
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
#define KCOV_TRACE_PC 0
#define KCOV_TRACE_CMP 1

static void burst(int n) {
    for (volatile int i = 0; i < n; i++) {
        getpid();
        getuid();
    }
}

int main(void) {
    TEST_START("KCOV §5/§6: ENABLE/DISABLE, re-enable cycles");

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

    /* §5: TRACE_CMP is rejected with EINVAL until CMP hooks are implemented */
    CHECK_ERR(ioctl(fd, KCOV_ENABLE, KCOV_TRACE_CMP), EINVAL,
              "ENABLE TRACE_CMP rejected (not yet implemented)");
    /* After failed ENABLE, mode is still INIT. DISABLE from INIT → EINVAL. */
    CHECK_ERR(ioctl(fd, KCOV_DISABLE, 0), EINVAL, "DISABLE after failed TRACE_CMP → EINVAL");

    /* §6: DISABLE before ENABLE → EINVAL (Linux: current->kcov != kcov) */
    CHECK_ERR(ioctl(fd, KCOV_DISABLE, 0), EINVAL, "DISABLE before ENABLE → EINVAL");

    /* §6: "After this call coverage can be enabled" — re-enable same thread */
    for (int cycle = 1; cycle <= 3; cycle++) {
        CHECK_RET(ioctl(fd, KCOV_ENABLE, KCOV_TRACE_PC), 0, "ENABLE");
        burst(50);
        CHECK_RET(ioctl(fd, KCOV_DISABLE, 0), 0, "DISABLE");
    }

    /* §6: coverage accumulated across cycles */
    CHECK(buf[0] >= 3, "accumulated coverage across 3 cycles");
    CHECK(buf[0] < 256, "accumulated coverage across 3 cycles");

    /* §6: after DISABLE, collection stops */
    uint64_t saved = buf[0];
    burst(100);
    CHECK(buf[0] == saved, "count unchanged after DISABLE (stopped)");

    printf("  INFO: final count=%lu\n", buf[0]);
    munmap(buf, sz);
    close(fd);
    TEST_DONE();
}
