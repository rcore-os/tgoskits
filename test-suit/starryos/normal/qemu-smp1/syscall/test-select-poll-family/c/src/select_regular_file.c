#include "test_framework.h"
#include "helpers.h"
#include <sys/select.h>
#include <sys/time.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_select_regular_file(void) {
    MODULE_START("select_regular_file");

    const char *path = "/tmp/test_select_regular";
    int wfd = open(path, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    CHECK(wfd >= 0, "open for write");
    char c = 'A';
    write_exact(wfd, &c, 1);
    close(wfd);

    int rfd = open(path, O_RDONLY);
    CHECK(rfd >= 0, "open for read");

    fd_set rfds;
    FD_ZERO(&rfds);
    FD_SET(rfd, &rfds);
    struct timeval tv = {1, 0};
    int ret = raw_select(rfd + 1, &rfds, NULL, NULL, &tv);
    CHECK(ret == 1, "select regular file readfds returns 1");

    fd_set wfds;
    FD_ZERO(&wfds);
    FD_SET(rfd, &wfds);
    tv.tv_sec = 1;
    tv.tv_usec = 0;
    ret = raw_select(rfd + 1, NULL, &wfds, NULL, &tv);
    CHECK(ret == 1, "select regular file writefds returns 1");

    close(rfd);
    unlink(path);

    MODULE_SUMMARY("select_regular_file");
    MODULE_RETURN();
}
