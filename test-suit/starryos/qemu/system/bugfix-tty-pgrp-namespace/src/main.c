#define _GNU_SOURCE
#include "test_framework.h"

#include <fcntl.h>
#include <pty.h>
#include <signal.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <termios.h>
#include <unistd.h>

/*
 * The system test runner gives each case its own PID namespace, so a foreground
 * group reported from the root namespace comes back as a number this process
 * never used.
 */
static char slave_name[64];

static void session_with_the_terminal(void)
{
    /* A session leader's own group is orphaned, so writing the terminal from
     * the background would fail; ignoring SIGTTOU is what shells do. */
    signal(SIGTTOU, SIG_IGN);
    CHECK(setsid() > 0, "start a session without a controlling terminal");
    int slave = open(slave_name, O_RDWR | O_NOCTTY);
    CHECK(slave >= 0, "open the slave");
    CHECK_RET(ioctl(slave, TIOCSCTTY, 0), 0, "make it the controlling terminal");
    CHECK_RET(tcgetpgrp(slave), getpgrp(), "the session leader's group is in the foreground");
    CHECK_RET(tcsetpgrp(slave, getpgrp()), 0, "set the foreground group to this one");
    CHECK_RET(tcgetpgrp(slave), getpgrp(), "reading it back gives the same number");

    int ready[2];
    CHECK_RET(pipe(ready), 0, "pipe for the child");
    pid_t child = fork();
    if (child == 0) {
        close(ready[0]);
        setpgid(0, 0);
        char byte = 'r';
        if (write(ready[1], &byte, 1) != 1)
            _exit(11);
        pause();
        _exit(0);
    }
    close(ready[1]);
    char byte = 0;
    CHECK_RET(read(ready[0], &byte, 1), 1, "the child joined its own group");
    close(ready[0]);

    CHECK_RET(tcsetpgrp(slave, child), 0, "hand the terminal to the child's group");
    CHECK_RET(tcgetpgrp(slave), child, "the terminal reports the child's group");
    CHECK_RET(getpgid(child), child, "getpgid agrees with what the terminal reports");
    CHECK_RET(tcsetpgrp(slave, getpgrp()), 0, "take the terminal back");
    kill(child, SIGKILL);
    int status = 0;
    CHECK_RET(waitpid(child, &status, 0), child, "reap the child");
}

int main(void)
{
    TEST_START("tty job control ids use the caller's PID namespace");

    int master = -1;
    int slave = -1;
    CHECK_RET(openpty(&master, &slave, slave_name, NULL, NULL), 0, "openpty");
    if (slave >= 0)
        close(slave);

    fflush(stdout);
    int failed_before = __fail;
    pid_t leader = fork();
    if (leader == 0) {
        session_with_the_terminal();
        fflush(stdout);
        _exit(__fail > failed_before);
    }
    int status = 0;
    CHECK_RET(waitpid(leader, &status, 0), leader, "the session runs to the end");
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "the session's checks all passed");
    close(master);

    TEST_DONE();
}
