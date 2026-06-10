#include "test_framework.h"

#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>

#define TMPFILE "/tmp/starry_test_lseek"
#define WRITE_STR "abcdefg"

struct seek_case {
    off_t off;
    int whence;
    const char *name;
    off_t expected_off;
    ssize_t expected_size;
    const char *expected_data;
};

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

    return fd;
}

static void verify_seek_case(int fd, const struct seek_case *tc)
{
    char buf[16];
    char msg[96];

    CHECK_RET(lseek(fd, 0, SEEK_END), strlen(WRITE_STR),
              "reset offset to end before lseek case");
    memset(buf, 0, sizeof(buf));

    snprintf(msg, sizeof(msg), "lseek %s returns expected offset", tc->name);
    CHECK_RET(lseek(fd, tc->off, tc->whence), tc->expected_off, msg);

    snprintf(msg, sizeof(msg), "read after lseek %s returns expected size", tc->name);
    CHECK_RET(read(fd, buf, (size_t)tc->expected_size), tc->expected_size, msg);
    if (tc->expected_data != NULL) {
        snprintf(msg, sizeof(msg), "read after lseek %s returns expected data", tc->name);
        CHECK(memcmp(buf, tc->expected_data, (size_t)tc->expected_size) == 0, msg);
    }
}

int main(void)
{
    TEST_START("lseek syscall semantics");

    unlink(TMPFILE);

    int fd = create_file_with_data(WRITE_STR, strlen(WRITE_STR));
    CHECK(fd >= 0, "create lseek fixture");
    if (fd >= 0) {
        const struct seek_case cases[] = {
            {4, SEEK_SET, "SEEK_SET", 4, 3, "efg"},
            {-2, SEEK_CUR, "SEEK_CUR", 5, 2, "fg"},
            {-4, SEEK_END, "SEEK_END -4", 3, 4, "defg"},
            {0, SEEK_END, "SEEK_END 0", 7, 0, NULL},
        };

        for (size_t i = 0; i < sizeof(cases) / sizeof(cases[0]); i++) {
            verify_seek_case(fd, &cases[i]);
        }

        CHECK_RET(close(fd), 0, "close lseek fixture");
    }

    fd = create_file_with_data("start", 5);
    CHECK(fd >= 0, "create sparse lseek fixture");
    if (fd >= 0) {
        struct stat st;
        char tail[4];

        CHECK_RET(lseek(fd, 1000, SEEK_CUR), 1005,
                  "lseek beyond EOF creates sparse position");
        CHECK_RET(write(fd, "end", 3), 3, "write after sparse seek");
        CHECK_RET(stat(TMPFILE, &st), 0, "stat sparse file");
        CHECK(st.st_size == 1008, "sparse file size includes hole");
        CHECK_RET(lseek(fd, 1005, SEEK_SET), 1005, "seek to data after hole");
        memset(tail, 0, sizeof(tail));
        CHECK_RET(read(fd, tail, 3), 3, "read data after sparse seek");
        CHECK(memcmp(tail, "end", 3) == 0, "data after sparse seek is preserved");
        CHECK_RET(close(fd), 0, "close sparse fixture");
    }

    fd = create_file_with_data("readonly", 8);
    CHECK(fd >= 0, "create read-only lseek fixture");
    if (fd >= 0) {
        CHECK_RET(close(fd), 0, "close read-only lseek fixture setup");
    }

    fd = openat(AT_FDCWD, TMPFILE, O_RDONLY);
    CHECK(fd >= 0, "open read-only lseek descriptor");
    if (fd >= 0) {
        CHECK_RET(lseek(fd, 0, SEEK_SET), 0, "lseek works on O_RDONLY fd");
        CHECK_RET(close(fd), 0, "close read-only lseek descriptor");
    }

    fd = openat(AT_FDCWD, TMPFILE, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    CHECK(fd >= 0, "open write-only lseek descriptor");
    if (fd >= 0) {
        CHECK_RET(write(fd, "test", 4), 4, "write data to O_WRONLY fd");
        CHECK_RET(lseek(fd, 0, SEEK_SET), 0, "lseek works on O_WRONLY fd");
        CHECK_RET(close(fd), 0, "close write-only lseek descriptor");
    }

    CHECK_ERR(lseek(-1, 0, SEEK_SET), EBADF, "lseek(-1) returns EBADF");

    fd = create_file_with_data("abc", 3);
    CHECK(fd >= 0, "create invalid lseek fixture");
    if (fd >= 0) {
        CHECK_ERR(lseek(fd, -1, SEEK_SET), EINVAL,
                  "negative SEEK_SET offset returns EINVAL");
        CHECK_ERR(lseek(fd, 0, 99), EINVAL, "invalid whence returns EINVAL");
        CHECK_RET(close(fd), 0, "close invalid lseek fixture");
    }

    int pipefd[2];
    errno = 0;
    int pipe_ret = pipe(pipefd);
    CHECK(pipe_ret == 0, "create pipe for lseek");
    if (pipe_ret == 0) {
        CHECK_ERR(lseek(pipefd[0], 0, SEEK_SET), ESPIPE,
                  "lseek on pipe returns ESPIPE");
        CHECK_RET(close(pipefd[0]), 0, "close pipe read end");
        CHECK_RET(close(pipefd[1]), 0, "close pipe write end");
    }

    unlink(TMPFILE);
    TEST_DONE();
}
