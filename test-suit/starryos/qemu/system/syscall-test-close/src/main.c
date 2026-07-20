#include "test_framework.h"

#include <fcntl.h>
#include <sys/socket.h>
#include <unistd.h>

#define TMPFILE "/tmp/starry_test_close"

int main(void)
{
    TEST_START("close syscall semantics");

    unlink(TMPFILE);

    int fd = openat(AT_FDCWD, TMPFILE, O_CREAT | O_RDWR | O_TRUNC, 0644);
    CHECK(fd >= 0, "open regular file");
    if (fd >= 0) {
        CHECK_RET(close(fd), 0, "close regular file succeeds");
        CHECK_ERR(write(fd, "x", 1), EBADF,
                  "write after closing file fd returns EBADF");
    }

    int pipefd[2];
    errno = 0;
    int pipe_ret = pipe(pipefd);
    CHECK(pipe_ret == 0, "create pipe");
    if (pipe_ret == 0) {
        CHECK_RET(close(pipefd[0]), 0, "close pipe read end succeeds");
        CHECK_RET(close(pipefd[1]), 0, "close pipe write end succeeds");
    }

    int sockfd = socket(AF_INET, SOCK_STREAM, 0);
    CHECK(sockfd >= 0, "create socket");
    if (sockfd >= 0) {
        CHECK_RET(close(sockfd), 0, "close socket succeeds");
    }

    CHECK_ERR(close(-1), EBADF, "close(-1) returns EBADF");
    CHECK_ERR(close(9999), EBADF, "close on unopened fd returns EBADF");

    fd = openat(AT_FDCWD, TMPFILE, O_CREAT | O_RDWR | O_TRUNC, 0644);
    CHECK(fd >= 0, "open double-close fixture");
    if (fd >= 0) {
        CHECK_RET(close(fd), 0, "first close succeeds");
        CHECK_ERR(close(fd), EBADF, "second close returns EBADF");
    }

    unlink(TMPFILE);
    TEST_DONE();
}
