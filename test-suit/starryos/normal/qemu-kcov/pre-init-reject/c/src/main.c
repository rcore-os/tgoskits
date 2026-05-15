/* kcov-spec §2a,§3a,§5a: Rejections before INIT_TRACE.
 * MUST run first — each test binary is a fresh process with clean TID. */
#include "test_framework.h"
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

#define KCOV_INIT_TRACE _IOR('c', 1, unsigned long)
#define KCOV_ENABLE _IO('c', 100)
#define KCOV_TRACE_PC 0

int main(void) {
    TEST_START("KCOV §2a/§3a/§5a: rejections before INIT_TRACE");

    /* §3a: mmap before INIT_TRACE → EINVAL (Linux: kcov->area == NULL) */
    {
        int fd = open("/dev/kcov", O_RDWR);
        CHECK(fd >= 0, "open for mmap-before-init test");
        size_t sz = 256 * sizeof(uint64_t);
        void *p = mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
        CHECK(p == MAP_FAILED, "mmap before INIT_TRACE rejected");
        if (p == MAP_FAILED)
            CHECK(errno == EINVAL, "mmap-before-init errno = EINVAL");
        close(fd);
    }

    /* §5a: ENABLE before INIT_TRACE → EINVAL (Linux: kcov->mode != INIT) */
    {
        int fd = open("/dev/kcov", O_RDWR);
        CHECK(fd >= 0, "open for enable-before-init test");
        CHECK_ERR(ioctl(fd, KCOV_ENABLE, KCOV_TRACE_PC), EINVAL,
                  "ENABLE before INIT_TRACE → EINVAL");
        close(fd);
    }

    TEST_DONE();
}
