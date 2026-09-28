#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/ioctl.h>
#include <sys/mount.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <poll.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

#define PTS_DIR "/tmp/tty-ctty-pts"

/* Each open pty costs a devpts index, so the cases share as few as possible. */
static char slave_name[64];
/* Children inherit the master, so they can type on the terminal they acquire. */
static int shared_master = -1;

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

static int open_pty(void)
{
    int master = open_private_master(PTS_DIR, slave_name, sizeof(slave_name));
    CHECK(master >= 0, "allocate a pty from a private devpts instance");
    return master;
}

/* Runs body in a forked child; the child's failed checks fail the named check here. */
static void in_child(const char *name, int new_session, void (*body)(void))
{
    fflush(stdout);
    int failed_before = __fail;
    pid_t child = fork();
    if (child < 0) {
        CHECK(0, name);
        return;
    }
    if (child == 0) {
        if (new_session)
            CHECK(setsid() > 0, "start a session without a controlling terminal");
        body();
        fflush(stdout);
        _exit(__fail > failed_before);
    }
    int status = 0;
    CHECK_RET(waitpid(child, &status, 0), child, name);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, name);
}

static void no_terminal_dev_tty_fails_enxio(void)
{
    CHECK_ERR(open("/dev/tty", O_RDWR), ENXIO, "/dev/tty without a controlling terminal fails ENXIO");
}

static void noctty_open_does_not_acquire(void)
{
    CHECK(open(slave_name, O_RDWR | O_NOCTTY) >= 0, "open the slave with O_NOCTTY");
    CHECK_ERR(open("/dev/tty", O_RDWR), ENXIO, "O_NOCTTY leaves the session without a terminal");
}

static void write_only_open_does_not_acquire(void)
{
    CHECK(open(slave_name, O_WRONLY) >= 0, "open the slave write-only");
    CHECK_ERR(open("/dev/tty", O_RDWR), ENXIO, "a write-only open does not acquire the terminal");
}

/* Linux reserves the descriptor before tty_open() runs, so an open refused
 * for want of one leaves the session without a terminal. */
static void emfile_open_does_not_acquire(void)
{
    int lowest_free = dup(0);
    CHECK(lowest_free >= 0, "find the lowest free descriptor");
    if (lowest_free < 0)
        return;
    close(lowest_free);
    struct rlimit saved;
    CHECK_RET(getrlimit(RLIMIT_NOFILE, &saved), 0, "read RLIMIT_NOFILE");
    struct rlimit full = saved;
    full.rlim_cur = (rlim_t)lowest_free;
    CHECK_RET(setrlimit(RLIMIT_NOFILE, &full), 0, "leave no descriptor free");
    CHECK_ERR(open(slave_name, O_RDWR), EMFILE, "opening the slave fails with EMFILE");
    CHECK_RET(setrlimit(RLIMIT_NOFILE, &saved), 0, "restore RLIMIT_NOFILE");
    CHECK_ERR(open("/dev/tty", O_RDWR), ENXIO, "the refused open left the session without a terminal");
}

static void master_open_does_not_acquire(void)
{
    int master = open(PTS_DIR "/ptmx", O_RDWR);
    CHECK(master >= 0, "open a pty master");
    CHECK_ERR(open("/dev/tty", O_RDWR), ENXIO, "a pty master never becomes the controlling terminal");
    if (master >= 0)
        close(master);
}

static void opener_in_child(void)
{
    CHECK(open(slave_name, O_RDWR) >= 0, "a process that is not a session leader opens the slave");
    CHECK_ERR(open("/dev/tty", O_RDWR), ENXIO, "only a session leader acquires a terminal by opening it");
}

static void non_leader_does_not_acquire(void)
{
    in_child("non-leader opener", 0, opener_in_child);
}

static void second_session_opener(void)
{
    CHECK(open(slave_name, O_RDWR) >= 0, "a second session opens the same slave");
    CHECK_ERR(open("/dev/tty", O_RDWR), ENXIO, "a terminal owned by another session is not taken over");
}

/* The sequence xterm runs in its child: open the slave by name, then /dev/tty. */
static void leader_open_acquires(void)
{
    CHECK(open(slave_name, O_RDWR) >= 0, "a session leader opens the slave without O_NOCTTY");
    int tty = open("/dev/tty", O_RDWR);
    CHECK(tty >= 0, "/dev/tty opens the acquired terminal");
    CHECK_RET(write(tty, "ok\n", 3), 3, "write through /dev/tty");
    /* Reading is what a background process group is refused, so it shows this group is in the foreground. */
    CHECK_RET(write(shared_master, "in\n", 3), 3, "type a line on the master");
    struct pollfd pfd = {.fd = tty, .events = POLLIN};
    CHECK_RET(poll(&pfd, 1, 5000), 1, "the typed line is readable on the terminal");
    char line[8] = {0};
    CHECK_RET(read(tty, line, sizeof(line) - 1), 3, "the session reads the typed line from its terminal");
    in_child("second session", 1, second_session_opener);
    CHECK(open("/dev/tty", O_RDWR) >= 0, "the terminal still belongs to this session");
}

int main(void)
{
    TEST_START("opening a tty sets the controlling terminal as Linux tty_open does");

    in_child("no controlling terminal", 1, no_terminal_dev_tty_fails_enxio);

    shared_master = open_pty();
    in_child("O_NOCTTY", 1, noctty_open_does_not_acquire);
    in_child("write-only open", 1, write_only_open_does_not_acquire);
    in_child("EMFILE open", 1, emfile_open_does_not_acquire);
    in_child("pty master", 1, master_open_does_not_acquire);
    in_child("leader acquires", 1, leader_open_acquires);

    char buf[32] = {0};
    struct pollfd pfd = {.fd = shared_master, .events = POLLIN};
    CHECK_RET(poll(&pfd, 1, 5000), 1, "the master has what the child wrote");
    CHECK(read(shared_master, buf, sizeof(buf) - 1) > 0 && strstr(buf, "ok") != NULL, "the master reads it");
    close(shared_master);

    int other = open_pty();
    in_child("not a session leader", 1, non_leader_does_not_acquire);
    close(other);

    TEST_DONE();
}
