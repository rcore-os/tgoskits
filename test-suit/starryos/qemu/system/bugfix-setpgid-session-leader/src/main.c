#define _GNU_SOURCE
#include "test_framework.h"

#include <signal.h>
#include <sys/wait.h>
#include <unistd.h>

/* A session leader already leads its own group; Linux refuses to move it. */
static void leader_cannot_change_its_group(void)
{
    CHECK(setsid() > 0, "start a new session");
    CHECK_RET(getpgrp(), getpid(), "the leader's group is its own pid");
    CHECK_ERR(setpgid(0, 0), EPERM, "setpgid(0, 0) on a session leader is refused");
    CHECK_ERR(setpgid(0, getpid()), EPERM, "so is naming the group it already leads");
    CHECK_ERR(setpgid(getpid(), 0), EPERM, "and naming itself by pid");
}

/* A process that is not a session leader may still start its own group. */
static void a_member_may_start_its_own_group(void)
{
    CHECK_RET(setpgid(0, 0), 0, "a non-leader creates its own group");
    CHECK_RET(getpgrp(), getpid(), "its group is its own pid");
}

static void in_child(const char *name, void (*body)(void))
{
    fflush(stdout);
    int failed_before = __fail;
    pid_t child = fork();
    if (child < 0) {
        CHECK(0, name);
        return;
    }
    if (child == 0) {
        body();
        fflush(stdout);
        _exit(__fail > failed_before);
    }
    int status = 0;
    CHECK_RET(waitpid(child, &status, 0), child, name);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, name);
}

int main(void)
{
    TEST_START("setpgid refuses a session leader");

    in_child("session leader", leader_cannot_change_its_group);
    in_child("group member", a_member_may_start_its_own_group);

    /* The parent sees the same refusal for a leader it created. */
    int ready[2];
    CHECK_RET(pipe(ready), 0, "pipe for the child");
    pid_t leader = fork();
    CHECK(leader >= 0, "fork a child to become a session leader");
    if (leader < 0) {
        TEST_DONE();
    }
    if (leader == 0) {
        close(ready[0]);
        setsid();
        char byte = 'r';
        if (write(ready[1], &byte, 1) != 1)
            _exit(11);
        pause();
        _exit(0);
    }
    close(ready[1]);
    char byte = 0;
    CHECK_RET(read(ready[0], &byte, 1), 1, "the child became a session leader");
    close(ready[0]);
    CHECK_ERR(setpgid(leader, leader), EPERM, "the parent cannot move a session leader either");
    kill(leader, SIGKILL);
    int status = 0;
    CHECK_RET(waitpid(leader, &status, 0), leader, "reap the child");

    TEST_DONE();
}
