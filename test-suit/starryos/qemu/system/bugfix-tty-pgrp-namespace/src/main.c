#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/ioctl.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sched.h>
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

static char slave_name[64];

/*
 * Linux answers tiocgpgrp() with pid_vnr(), which is 0 for a group the caller's
 * namespace cannot name; it does not report an error.
 */
static void group_outside_the_namespace_reads_as_zero(int slave)
{
    fflush(stdout);
    int failed_before = __fail;
    pid_t child = fork();
    if (child == 0) {
        if (unshare(CLONE_NEWPID) != 0) {
            _exit(12);
        }
        pid_t inner = fork();
        if (inner == 0) {
            CHECK_RET(tcgetpgrp(slave), 0, "a foreground group this namespace cannot name reads as 0");
            fflush(stdout);
            _exit(__fail > failed_before);
        }
        int inner_status = 0;
        if (waitpid(inner, &inner_status, 0) != inner) {
            _exit(13);
        }
        _exit(WIFEXITED(inner_status) ? WEXITSTATUS(inner_status) : 14);
    }
    int status = 0;
    CHECK_RET(waitpid(child, &status, 0), child, "the nested namespace runs to the end");
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "the terminal reports 0 to a namespace that cannot name the group");
}

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
    group_outside_the_namespace_reads_as_zero(slave);
    kill(child, SIGKILL);
    int status = 0;
    CHECK_RET(waitpid(child, &status, 0), child, "reap the child");
}

int main(void)
{
    TEST_START("tty job control ids use the caller's PID namespace");

    int master = open_private_master("/tmp/tty-pgrp-pts", slave_name, sizeof(slave_name));
    CHECK(master >= 0, "allocate a pty from a private devpts instance");

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
