#include "test_framework.h"

#include <fcntl.h>
#include <unistd.h>

#define TMPFILE "/tmp/starry_test_read"
#define READ_PATTERN_SIZE 512

static void fill_pattern(char *buf, size_t size)
{
    for (size_t i = 0; i < size; i++) {
        buf[i] = (char)('A' + (i % 26));
    }
}

static int create_file_with_data(const char *data, size_t len)
{
    int fd = openat(AT_FDCWD, TMPFILE, O_CREAT | O_RDWR | O_TRUNC, 0644);
    if (fd < 0) {
        return -1;
    }

    if (len > 0) {
        ssize_t written = write(fd, data, len);
        if (written != (ssize_t)len) {
            close(fd);
            return -1;
        }
    }

    if (lseek(fd, 0, SEEK_SET) != 0) {
        close(fd);
        return -1;
    }

    return fd;
}

int main(void)
{
    TEST_START("read syscall semantics");

    unlink(TMPFILE);

    char pattern[READ_PATTERN_SIZE];
    char buf[READ_PATTERN_SIZE];
    fill_pattern(pattern, sizeof(pattern));

    int fd = create_file_with_data(pattern, sizeof(pattern));
    CHECK(fd >= 0, "create 512-byte fixture");
    if (fd >= 0) {
        memset(buf, 0, sizeof(buf));
        CHECK_RET(read(fd, buf, sizeof(buf)), READ_PATTERN_SIZE,
                  "read returns the full requested fixture");
        CHECK(memcmp(buf, pattern, sizeof(pattern)) == 0,
              "read data matches fixture bytes");
        CHECK_RET(lseek(fd, 0, SEEK_CUR), READ_PATTERN_SIZE,
                  "read advances file offset by bytes read");
        CHECK_RET(read(fd, buf, sizeof(buf)), 0, "read at EOF returns 0");
        CHECK_RET(close(fd), 0, "close read fixture");
    }

    fd = create_file_with_data("hello", 5);
    CHECK(fd >= 0, "create short fixture");
    if (fd >= 0) {
        char partial[100];
        memset(partial, 0, sizeof(partial));
        CHECK_RET(read(fd, partial, sizeof(partial)), 5,
                  "read returns remaining bytes when request is larger");
        CHECK(memcmp(partial, "hello", 5) == 0,
              "partial read copies available bytes");
        CHECK_RET(close(fd), 0, "close short fixture");
    }

    fd = create_file_with_data(NULL, 0);
    CHECK(fd >= 0, "create empty fixture");
    if (fd >= 0) {
        char empty[8];
        CHECK_RET(read(fd, empty, sizeof(empty)), 0,
                  "read from empty file returns EOF immediately");
        CHECK_RET(close(fd), 0, "close empty fixture");
    }

    fd = create_file_with_data("abc", 3);
    CHECK(fd >= 0, "create zero-length read fixture");
    if (fd >= 0) {
        CHECK_RET(read(fd, buf, 0), 0, "read count 0 returns 0");
        CHECK_RET(lseek(fd, 0, SEEK_CUR), 0,
                  "read count 0 leaves file offset unchanged");
        CHECK_RET(close(fd), 0, "close zero-length fixture");
    }

    char dummy[4];
    CHECK_ERR(read(-1, dummy, 1), EBADF, "read(-1) returns EBADF");
    CHECK_ERR(read(9999, dummy, 1), EBADF, "read on unopened fd returns EBADF");

    fd = openat(AT_FDCWD, TMPFILE, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    CHECK(fd >= 0, "open write-only file");
    if (fd >= 0) {
        CHECK_ERR(read(fd, dummy, 1), EBADF,
                  "read from O_WRONLY descriptor returns EBADF");
        CHECK_RET(close(fd), 0, "close write-only descriptor");
    }

    unlink(TMPFILE);
    TEST_DONE();
}
