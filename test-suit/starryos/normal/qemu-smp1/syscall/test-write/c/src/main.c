#include "test_framework.h"

#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>

#define TMPFILE "/tmp/starry_test_write"
#define LARGE_WRITE_SIZE (1024 * 1024)

static void fill_pattern(char *buf, size_t size, unsigned char seed)
{
    for (size_t i = 0; i < size; i++) {
        buf[i] = (char)((i + seed) & 0xff);
    }
}

static int open_tmp_rw(void)
{
    return openat(AT_FDCWD, TMPFILE, O_CREAT | O_RDWR | O_TRUNC, 0644);
}

int main(void)
{
    TEST_START("write syscall semantics");

    unlink(TMPFILE);

    int fd = open_tmp_rw();
    CHECK(fd >= 0, "open write fixture");
    if (fd >= 0) {
        const char *msg = "Hello StarryOS!";
        size_t msg_len = strlen(msg);
        char buf[64];

        CHECK_RET(write(fd, msg, msg_len), msg_len, "write stores full buffer");
        CHECK_RET(lseek(fd, 0, SEEK_CUR), msg_len,
                  "write advances file offset by bytes written");

        CHECK_RET(lseek(fd, 0, SEEK_SET), 0, "rewind after write");
        memset(buf, 0, sizeof(buf));
        CHECK_RET(read(fd, buf, msg_len), msg_len, "read written bytes back");
        CHECK(memcmp(buf, msg, msg_len) == 0, "written data is preserved");
        CHECK_RET(close(fd), 0, "close write fixture");

        struct stat st;
        CHECK_RET(stat(TMPFILE, &st), 0, "stat written file");
        CHECK(st.st_size == (off_t)msg_len, "stat observes write file size");
    }

    fd = open_tmp_rw();
    CHECK(fd >= 0, "open count fixture");
    if (fd >= 0) {
        const size_t sizes[] = {0, 1, 2, 7, 512, 4096, 8192};
        char two_pages[8192];
        int bad_count = 0;
        fill_pattern(two_pages, sizeof(two_pages), 0x31);

        for (size_t i = 0; i < sizeof(sizes) / sizeof(sizes[0]); i++) {
            CHECK_RET(lseek(fd, 0, SEEK_SET), 0, "reset offset before count write");
            errno = 0;
            ssize_t ret = write(fd, two_pages, sizes[i]);
            if (ret != (ssize_t)sizes[i]) {
                printf("  INFO | write size %ld returned %ld errno=%d (%s)\n",
                       (long)sizes[i], (long)ret, errno, strerror(errno));
                bad_count++;
            }
        }
        CHECK(bad_count == 0, "write returns requested count for common sizes");
        CHECK_RET(close(fd), 0, "close count fixture");
    }

    fd = open_tmp_rw();
    CHECK(fd >= 0, "open overwrite fixture");
    if (fd >= 0) {
        char buf[8];

        CHECK_RET(write(fd, "AAAA", 4), 4, "write original bytes");
        CHECK_RET(lseek(fd, 2, SEEK_SET), 2, "seek to overwrite position");
        CHECK_RET(write(fd, "BB", 2), 2, "write overwrites existing bytes");
        CHECK_RET(lseek(fd, 0, SEEK_SET), 0, "rewind overwritten file");
        memset(buf, 0, sizeof(buf));
        CHECK_RET(read(fd, buf, sizeof(buf)), 4, "read overwritten file");
        CHECK(memcmp(buf, "AABB", 4) == 0, "overwrite result is AABB");
        CHECK_RET(close(fd), 0, "close overwrite fixture");
    }

    fd = open_tmp_rw();
    CHECK(fd >= 0, "open large write fixture");
    if (fd >= 0) {
        char *big_buf = malloc(LARGE_WRITE_SIZE);
        char *big_read = malloc(LARGE_WRITE_SIZE);
        CHECK(big_buf != NULL && big_read != NULL, "allocate large buffers");
        if (big_buf != NULL && big_read != NULL) {
            fill_pattern(big_buf, LARGE_WRITE_SIZE, 0x42);
            memset(big_read, 0, LARGE_WRITE_SIZE);
            CHECK_RET(write(fd, big_buf, LARGE_WRITE_SIZE), LARGE_WRITE_SIZE,
                      "write 1 MiB buffer");
            CHECK_RET(lseek(fd, 0, SEEK_SET), 0, "rewind large file");
            CHECK_RET(read(fd, big_read, LARGE_WRITE_SIZE), LARGE_WRITE_SIZE,
                      "read 1 MiB buffer back");
            CHECK(memcmp(big_buf, big_read, LARGE_WRITE_SIZE) == 0,
                  "large write data matches byte-for-byte");
        }
        free(big_read);
        free(big_buf);
        CHECK_RET(close(fd), 0, "close large write fixture");
    }

    CHECK_ERR(write(-1, "x", 1), EBADF, "write(-1) returns EBADF");
    CHECK_ERR(write(9999, "x", 1), EBADF, "write on unopened fd returns EBADF");

    fd = open_tmp_rw();
    CHECK(fd >= 0, "create read-only fixture");
    if (fd >= 0) {
        CHECK_RET(close(fd), 0, "close read-only fixture setup");
    }

    fd = openat(AT_FDCWD, TMPFILE, O_RDONLY);
    CHECK(fd >= 0, "open read-only descriptor");
    if (fd >= 0) {
        CHECK_ERR(write(fd, "x", 1), EBADF,
                  "write to O_RDONLY descriptor returns EBADF");
        CHECK_RET(close(fd), 0, "close read-only descriptor");
    }

    unlink(TMPFILE);
    TEST_DONE();
}
