/* kcov-spec §2: KCOV_INIT_TRACE — size bounds, second call */
#include "test_framework.h"
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/ioctl.h>
#include <unistd.h>

#define KCOV_INIT_TRACE _IOR('c', 1, unsigned long)
#define KCOV_MAX_ENTRIES (1024 * 1024)

int main(void) {
    TEST_START("KCOV §2: INIT_TRACE size bounds and repeat");

    int fd = open("/dev/kcov", O_RDWR);
    CHECK(fd >= 0, "open");

    /* Doc: size=0 → EINVAL */
    CHECK_ERR(ioctl(fd, KCOV_INIT_TRACE, 0), EINVAL, "size=0 → EINVAL");
    /* Doc: size>max → EINVAL */
    CHECK_ERR(ioctl(fd, KCOV_INIT_TRACE, KCOV_MAX_ENTRIES + 1), EINVAL,
              "size>max → EINVAL");

    /* Valid sizes */
    CHECK_RET(ioctl(fd, KCOV_INIT_TRACE, 64), 0, "size=64 accepted");
    close(fd);

    /* Second INIT_TRACE: Linux returns EBUSY. */
    {
        int fd2 = open("/dev/kcov", O_RDWR);
        CHECK_RET(ioctl(fd2, KCOV_INIT_TRACE, 128), 0, "first INIT_TRACE");
        CHECK_ERR(ioctl(fd2, KCOV_INIT_TRACE, 256), EBUSY,
                  "second INIT_TRACE → EBUSY (Linux spec)");
        close(fd2);
    }

    TEST_DONE();
}
