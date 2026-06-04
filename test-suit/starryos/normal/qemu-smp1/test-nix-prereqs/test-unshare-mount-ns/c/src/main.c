#define _GNU_SOURCE
#include <errno.h>
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

#define FAIL(msg)                                                              \
    do {                                                                       \
        fprintf(stderr, "FAIL | %s:%d | %s: %s\n", __FILE__, __LINE__, msg,    \
                strerror(errno));                                              \
        exit(1);                                                               \
    } while (0)

#define PASS(msg)                                                              \
    do {                                                                       \
        printf("  PASS | %s:%d | %s\n", __FILE__, __LINE__, msg);             \
    } while (0)

int main(void) {
    printf("================================================\n");
    printf("  TEST: unshare(CLONE_NEWNS) smoke\n");
    printf("================================================\n");

    pid_t child = fork();
    if (child < 0)
        FAIL("fork");

    if (child == 0) {
        if (unshare(CLONE_NEWNS) < 0)
            FAIL("unshare(CLONE_NEWNS)");
        PASS("child unshared mount namespace");
        exit(0);
    }

    {
        int status;
        if (waitpid(child, &status, 0) < 0)
            FAIL("waitpid child");
        if (!WIFEXITED(status) || WEXITSTATUS(status) != 0)
            FAIL("child exited non-zero");
        PASS("child exited cleanly");
    }

    printf("UNSHARE_MOUNT_NS_ALL_PASSED\n");
    return 0;
}
