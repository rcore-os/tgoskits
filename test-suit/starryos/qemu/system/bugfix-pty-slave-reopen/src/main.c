#define _GNU_SOURCE
#include "test_framework.h"

#include <fcntl.h>
#include <poll.h>
#include <pty.h>
#include <string.h>
#include <unistd.h>

/*
 * openpty() opens the slave and callers such as xterm close it, then reopen it
 * by name in the child. The master must forget the earlier hangup at that point.
 */
int main(void)
{
    TEST_START("reopening a pty slave clears the hangup seen by the master");

    int master = -1;
    int slave = -1;
    char name[64];
    CHECK_RET(openpty(&master, &slave, name, NULL, NULL), 0, "openpty");
    CHECK_RET(close(slave), 0, "close the only slave descriptor");

    int reopened = open(name, O_RDWR | O_NOCTTY);
    CHECK(reopened >= 0, "reopen the slave by name");

    struct pollfd pfd = {.fd = master, .events = POLLIN};
    CHECK_RET(poll(&pfd, 1, 100), 0, "the master has nothing to report while the slave is open again");

    char buf[32];
    CHECK_RET(fcntl(master, F_SETFL, O_NONBLOCK), 0, "make the master non-blocking");
    CHECK_ERR(read(master, buf, sizeof(buf)), EAGAIN, "an empty master read would block instead of reporting EOF");
    CHECK_RET(fcntl(master, F_SETFL, 0), 0, "make the master blocking again");

    CHECK_RET(write(reopened, "abc", 3), 3, "write through the reopened slave");
    pfd.revents = 0;
    CHECK_RET(poll(&pfd, 1, 1000), 1, "the master sees the data");
    memset(buf, 0, sizeof(buf));
    CHECK_RET(read(master, buf, sizeof(buf) - 1), 3, "the master reads what the reopened slave wrote");
    CHECK(strcmp(buf, "abc") == 0, "the bytes arrive unchanged");

    CHECK_RET(close(reopened), 0, "close the reopened slave");
    pfd.revents = 0;
    CHECK_RET(poll(&pfd, 1, 1000), 1, "the master reports the new hangup");
    errno = 0;
    ssize_t n = read(master, buf, sizeof(buf));
    CHECK(n == 0 || (n < 0 && errno == EIO), "the master read completes after the last slave close");

    close(master);
    TEST_DONE();
}
