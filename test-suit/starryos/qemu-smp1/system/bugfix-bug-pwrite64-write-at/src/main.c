#include "test_framework.h"

#include <fcntl.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#define TMPFILE "/tmp/starry_test_pwrite64"

static int create_fixture(const char *data)
{
    int fd = open(TMPFILE, O_CREAT | O_RDWR | O_TRUNC, 0644);
    if (fd < 0) {
        return -1;
    }
    if (data != NULL) {
        size_t len = strlen(data);
        if (write(fd, data, len) != (ssize_t)len) {
            close(fd);
            return -1;
        }
    }
    return fd;
}

int main(void)
{
    TEST_START("bug-pwrite64-write-at");

    unlink(TMPFILE);

    int fd = create_fixture("0123456789");
    CHECK(fd >= 0, "create pwrite64 fixture");
    if (fd >= 0) {
        char buf[16] = {0};
        struct stat st;

        CHECK_RET(lseek(fd, 2, SEEK_SET), 2, "set current offset before pwrite64");
        CHECK_RET(syscall(SYS_pwrite64, fd, "ABCD", 4, 4), 4,
                  "pwrite64 writes requested bytes at explicit offset");
        CHECK_RET(lseek(fd, 0, SEEK_CUR), 2,
                  "pwrite64 does not change current file offset");
        CHECK_RET(syscall(SYS_pread64, fd, buf, 10, 0), 10,
                  "read back pwrite64 fixture");
        CHECK(memcmp(buf, "0123ABCD89", 10) == 0,
              "pwrite64 overwrote only the requested range");

        CHECK_RET(syscall(SYS_pwrite64, fd, "ZZ", 2, 12), 2,
                  "pwrite64 grows file when writing past EOF");
        CHECK_RET(fstat(fd, &st), 0, "stat file after pwrite64 growth");
        CHECK(st.st_size == 14, "pwrite64 growth updates file size");

        memset(buf, 0, sizeof(buf));
        CHECK_RET(syscall(SYS_pread64, fd, buf, 2, 12), 2,
                  "read pwrite64 bytes written past EOF");
        CHECK(memcmp(buf, "ZZ", 2) == 0, "pwrite64 data past EOF matches");

        CHECK_RET(syscall(SYS_pwrite64, fd, "ignored", 0, 0), 0,
                  "pwrite64 with count 0 returns 0");
        CHECK_RET(close(fd), 0, "close pwrite64 fixture");
    }

    CHECK_ERR(syscall(SYS_pwrite64, -1, "x", 1, 0), EBADF,
              "pwrite64 on -1 returns EBADF");
    CHECK_ERR(syscall(SYS_pwrite64, 9999, "x", 1, 0), EBADF,
              "pwrite64 on unopened fd returns EBADF");

    fd = create_fixture("readonly");
    CHECK(fd >= 0, "create read-only pwrite64 fixture");
    if (fd >= 0) {
        CHECK_RET(close(fd), 0, "close setup descriptor");
    }

    fd = open(TMPFILE, O_RDONLY);
    CHECK(fd >= 0, "open read-only descriptor");
    if (fd >= 0) {
        CHECK_ERR(syscall(SYS_pwrite64, fd, "x", 1, 0), EBADF,
                  "pwrite64 on O_RDONLY fd returns EBADF");
        CHECK_RET(close(fd), 0, "close read-only descriptor");
    }

    unlink(TMPFILE);
    TEST_DONE();
}
