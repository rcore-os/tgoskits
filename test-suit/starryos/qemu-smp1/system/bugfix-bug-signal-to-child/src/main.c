/*
 * bug-signal-to-child: Parent sends SIGUSR1 to child via kill(),
 * child should receive it and wake from pause().
 *
 * Uses a pipe for synchronization: child writes "ready" before calling
 * pause(), parent reads it before sending the signal.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static volatile sig_atomic_t got_signal = 0;
static void handler(int sig) { (void)sig; got_signal = 1; }

int main(void)
{
    printf("=== bug-signal-to-child ===\n");
    printf("Expected: parent kill(child, SIGUSR1) wakes child from pause()\n\n");

    int sync_pipe[2];
    if (pipe(sync_pipe) != 0) {
        printf("FAIL: pipe: %s\n", strerror(errno));
        printf("TEST FAILED\n");
        return 1;
    }

    pid_t pid = fork();
    if (pid < 0) {
        printf("FAIL: fork: %s\n", strerror(errno));
        printf("TEST FAILED\n");
        return 1;
    }

    if (pid == 0) {
        /* child */
        close(sync_pipe[0]); /* close read end */

        struct sigaction sa = {0};
        sa.sa_handler = handler;
        sigemptyset(&sa.sa_mask);
        sigaction(SIGUSR1, &sa, NULL);
        got_signal = 0;

        /* tell parent we're ready */
        write(sync_pipe[1], "R", 1);
        close(sync_pipe[1]);

        /* wait for signal */
        int pr = pause();
        /* if we get here, pause returned */
        _exit(got_signal == 1 ? 0 : 2 + (pr == -1 ? errno : 100 + pr));
    }

    /* parent */
    close(sync_pipe[1]); /* close write end */

    /* wait for child to be ready */
    char buf;
    read(sync_pipe[0], &buf, 1);
    close(sync_pipe[0]);

    /* check if child is still alive */
    int status_check = 0;
    pid_t w_check = waitpid(pid, &status_check, WNOHANG);
    if (w_check == 0) {
        printf("parent: child is still running (good)\n");
    } else if (w_check == pid) {
        printf("parent: child already exited with code %d BEFORE signal!\n",
               WIFEXITED(status_check) ? WEXITSTATUS(status_check) : -1);
        printf("TEST FAILED\n");
        return 1;
    } else {
        printf("parent: waitpid(WNOHANG) returned %d errno=%d\n", w_check, errno);
    }

    /* small extra delay to ensure child is in pause() */
    struct timespec ts = {0, 100000000}; /* 100ms */
    nanosleep(&ts, NULL);

    printf("parent: sending SIGUSR1 to child %d\n", pid);
    int kr = kill(pid, SIGUSR1);
    printf("parent: kill returned %d (errno=%d)\n", kr, errno);

    int status = 0;
    pid_t w = waitpid(pid, &status, 0);

    if (w == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0) {
        printf("PASS: child received signal and exited 0\n");
        printf("TEST PASSED\n");
        return 0;
    }

    if (w != pid) {
        printf("FAIL: waitpid returned %d, expected %d (errno=%d)\n", w, pid, errno);
    } else if (!WIFEXITED(status)) {
        printf("FAIL: child killed by signal %d\n", WTERMSIG(status));
    } else {
        printf("FAIL: child exited with code %d (expected 0)\n", WEXITSTATUS(status));
    }
    printf("TEST FAILED\n");
    return 1;
}
