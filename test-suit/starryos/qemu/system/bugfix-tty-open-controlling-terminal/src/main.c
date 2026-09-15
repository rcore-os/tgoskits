#define _GNU_SOURCE
#include "test_framework.h"

#include <fcntl.h>
#include <poll.h>
#include <pty.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

/* Each open pty costs a devpts index, so the cases share as few as possible. */
static char slave_name[64];
/* Children inherit the master, so they can type on the terminal they acquire. */
static int shared_master = -1;

static int open_pty(void)
{
    int master = -1;
    int slave = -1;
    CHECK_RET(openpty(&master, &slave, slave_name, NULL, NULL), 0, "openpty");
    if (slave >= 0)
        close(slave);
    return master;
}

/* Runs body in a forked child; the child's failed checks fail the named check here. */
static void in_child(const char *name, int new_session, void (*body)(void))
{
    fflush(stdout);
    int failed_before = __fail;
    pid_t child = fork();
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

static void master_open_does_not_acquire(void)
{
    int master = posix_openpt(O_RDWR);
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
