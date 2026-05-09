#define _POSIX_C_SOURCE 200809L

#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static volatile sig_atomic_t seen_alarm;

static void on_alarm(int signo)
{
    (void)signo;
    seen_alarm = 1;
}

int main(void)
{
    struct sigaction sa;
    int rc;

    printf("=== bug-alarm-pause ===\n");
    printf("Expected: alarm-delivered SIGALRM wakes pause() with EINTR\n\n");

    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = on_alarm;
    sa.sa_flags = SA_RESTART;
    sigemptyset(&sa.sa_mask);
    if (sigaction(SIGALRM, &sa, NULL) != 0) {
        printf("FAIL: sigaction(SIGALRM): %s\n", strerror(errno));
        printf("TEST FAILED\n");
        return 1;
    }

    alarm(1);
    errno = 0;
    rc = pause();
    alarm(0);

    if (rc != -1) {
        printf("FAIL: pause returned %d, expected -1\n", rc);
        printf("TEST FAILED\n");
        return 1;
    }
    if (errno != EINTR) {
        printf("FAIL: pause errno=%d (%s), expected EINTR\n", errno, strerror(errno));
        printf("TEST FAILED\n");
        return 1;
    }
    if (!seen_alarm) {
        printf("FAIL: SIGALRM handler did not run\n");
        printf("TEST FAILED\n");
        return 1;
    }

    printf("PASS: pause woke after SIGALRM with EINTR\n");
    printf("TEST PASSED\n");
    return 0;
}
