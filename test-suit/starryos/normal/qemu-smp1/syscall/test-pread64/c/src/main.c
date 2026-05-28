#include "test_framework.h"

#include <fcntl.h>
#include <sys/syscall.h>
#include <unistd.h>

#define TMPFILE "/tmp/starry_test_pread64"

static int create_fixture(void)
{
    static const char data[] = "0123456789abcdef";
    int fd = open(TMPFILE, O_CREAT | O_RDWR | O_TRUNC, 0644);
    if (fd < 0) {
        return -1;
    }
    if (write(fd, data, sizeof(data) - 1) != (ssize_t)(sizeof(data) - 1)) {
        close(fd);
        return -1;
    }
    return fd;
}

int main(void)
{
    TEST_START("pread64 syscall semantics");

    unlink(TMPFILE);

    int fd = create_fixture();
    CHECK(fd >= 0, "create pread64 fixture");
    if (fd >= 0) {
        char buf[8] = {0};

        CHECK_RET(lseek(fd, 3, SEEK_SET), 3, "set current offset before pread64");
        CHECK_RET(syscall(SYS_pread64, fd, buf, 4, 8), 4,
                  "pread64 reads requested bytes from explicit offset");
        CHECK(memcmp(buf, "89ab", 4) == 0, "pread64 data matches offset");
        CHECK_RET(lseek(fd, 0, SEEK_CUR), 3,
                  "pread64 does not change current file offset");

        memset(buf, 0, sizeof(buf));
        CHECK_RET(syscall(SYS_pread64, fd, buf, 4, 16), 0,
                  "pread64 at EOF returns 0");
        CHECK_RET(syscall(SYS_pread64, fd, buf, 0, 0), 0,
                  "pread64 with count 0 returns 0");

        CHECK_RET(close(fd), 0, "close pread64 fixture");
    }

    char dummy[4];
    CHECK_ERR(syscall(SYS_pread64, -1, dummy, 1, 0), EBADF,
              "pread64 on -1 returns EBADF");
    CHECK_ERR(syscall(SYS_pread64, 9999, dummy, 1, 0), EBADF,
              "pread64 on unopened fd returns EBADF");

    fd = open(TMPFILE, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    CHECK(fd >= 0, "open write-only fixture");
    if (fd >= 0) {
        CHECK_ERR(syscall(SYS_pread64, fd, dummy, 1, 0), EBADF,
                  "pread64 on O_WRONLY fd returns EBADF");
        CHECK_RET(close(fd), 0, "close write-only fixture");
    }

    unlink(TMPFILE);
    TEST_DONE();
}
