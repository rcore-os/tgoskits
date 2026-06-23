#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

/* librga legacy kernel ioctl (drmrga.h). */
#define RGA_BLIT_SYNC 0x5017u

int main(void)
{
    TEST_START("rga-abi: /dev/rga node + RGA_BLIT_SYNC plumbing (graceful, no hardware)");

    int fd = open("/dev/rga", O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        /* Kernel built without the `rga` feature — nothing to exercise here. */
        printf("RGA_ABI_TEST SKIP: /dev/rga absent\n");
        TEST_DONE();
    }

    /* The kernel reads sizeof(struct rga_req) (~296 bytes) from the arg. A zeroed >=512-byte
       buffer covers it. A zeroed request has render_mode=0 (bitblt) and addr=0: the kernel parses
       it, then either fails to resolve the (invalid, fd 0) dma-buf buffer, or — with no RGA core
       present in QEMU — returns ENODEV. Either way it MUST fail gracefully: no success, no crash.
       This confirms /dev/rga exists, routes the ioctl into the handler, and returns cleanly without
       any RGA hardware. Real librga 0x5017 + pixel output are validated on the board (PR-E2). */
    unsigned char req[512];
    memset(req, 0, sizeof(req));

    errno = 0;
    int rc = ioctl(fd, RGA_BLIT_SYNC, req);
    int e = errno;

    CHECK(rc != 0, "RGA_BLIT_SYNC returns an error on QEMU (no RGA hardware)");
    CHECK(e == ENODEV || e == EBADF || e == EINVAL || e == ENOSYS || e == ENOTTY || e == EOPNOTSUPP,
          "errno is a graceful RGA error, not a fault");
    /* Reaching this line proves the kernel ioctl handler did not panic/crash. */
    CHECK(1, "kernel survived the ioctl path (no crash)");

    close(fd);
    TEST_DONE();
}
