#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/ioctl.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <poll.h>
#include <string.h>
#include <termios.h>
#include <unistd.h>

/*
 * openpty() opens the slave and callers such as xterm close it, then reopen it
 * by name in the child. The master must forget the earlier hangup at that point.
 */

/*
 * The shared /dev/pts table holds sixteen indices for the whole suite, so this
 * case allocates from an instance of its own.
 */
static int open_private_master(const char *mountpoint, char *slave, size_t len)
{
    static int mounted;
    if (!mounted) {
        if (mkdir(mountpoint, 0755) != 0 && errno != EEXIST) {
            return -1;
        }
        if (mount("none", mountpoint, "devpts", 0,
                  "newinstance,mode=0620,gid=5,ptmxmode=0666") != 0) {
            return -1;
        }
        mounted = 1;
    }

    char ptmx[96];
    snprintf(ptmx, sizeof(ptmx), "%s/ptmx", mountpoint);
    int master = open(ptmx, O_RDWR | O_NOCTTY);
    if (master < 0) {
        return -1;
    }
    unsigned int number = 0;
    int unlock = 0;
    if (ioctl(master, TIOCGPTN, &number) != 0 || ioctl(master, TIOCSPTLCK, &unlock) != 0) {
        close(master);
        return -1;
    }
    snprintf(slave, len, "%s/%u", mountpoint, number);
    return master;
}

int main(void)
{
    TEST_START("reopening a pty slave clears the hangup seen by the master");

    char name[64];
    int master = open_private_master("/tmp/pty-slave-reopen-pts", name, sizeof(name));
    CHECK(master >= 0, "allocate a pty from a private devpts instance");
    if (master < 0) {
        TEST_DONE();
    }
    int slave = open(name, O_RDWR | O_NOCTTY);
    CHECK(slave >= 0, "open the slave the first time");
    if (slave < 0) {
        TEST_DONE();
    }
    CHECK_RET(close(slave), 0, "close the only slave descriptor");

    int reopened = open(name, O_RDWR | O_NOCTTY);
    CHECK(reopened >= 0, "reopen the slave by name");
    if (reopened < 0) {
        TEST_DONE();
    }

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

    /* The other direction, with echo off so the master's queue stays empty. */
    struct termios mode;
    CHECK_RET(tcgetattr(reopened, &mode), 0, "read the slave's line settings");
    mode.c_lflag &= ~(tcflag_t)ECHO;
    CHECK_RET(tcsetattr(reopened, TCSANOW, &mode), 0, "turn echo off on the slave");
    CHECK_RET(write(master, "xyz\n", 4), 4, "the master types a line");
    struct pollfd spfd = {.fd = reopened, .events = POLLIN};
    CHECK_RET(poll(&spfd, 1, 1000), 1, "the reopened slave sees the line");
    memset(buf, 0, sizeof(buf));
    CHECK_RET(read(reopened, buf, sizeof(buf) - 1), 4, "the reopened slave reads what the master typed");
    CHECK(strcmp(buf, "xyz\n") == 0, "the line arrives unchanged");

    CHECK_RET(close(reopened), 0, "close the reopened slave");
    pfd.revents = 0;
    CHECK_RET(poll(&pfd, 1, 1000), 1, "the master reports the new hangup");
    errno = 0;
    ssize_t n = read(master, buf, sizeof(buf));
    CHECK(n == 0 || (n < 0 && errno == EIO), "the master read completes after the last slave close");

    close(master);
    TEST_DONE();
}
