#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static volatile sig_atomic_t g_alarm_fired;
static volatile sig_atomic_t g_child_pid;

static void alarm_kill_child(int sig)
{
    (void)sig;
    g_alarm_fired = 1;
    if (g_child_pid > 0) {
        kill((pid_t)g_child_pid, SIGKILL);
    }
}

static void install_alarm_handler(int flags)
{
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = alarm_kill_child;
    sa.sa_flags = flags;
    sigemptyset(&sa.sa_mask);
    CHECK_RET(sigaction(SIGALRM, &sa, NULL), 0, "install SIGALRM handler");
}

static pid_t fork_sleep_child(void)
{
    pid_t child = fork();
    CHECK(child >= 0, "fork sleep child");
    if (child == 0) {
        for (;;) {
            sleep(30);
        }
    }
    g_child_pid = child;
    g_alarm_fired = 0;
    return child;
}

static void reap_after_interrupt(pid_t child)
{
    int status = 0;
    pid_t got = waitpid(child, &status, 0);
    CHECK_RET(got, child, "waitpid reaps killed child after EINTR");
    CHECK(WIFSIGNALED(status), "child terminated by signal after EINTR");
    if (WIFSIGNALED(status)) {
        CHECK(WTERMSIG(status) == SIGKILL, "child terminated by SIGKILL after EINTR");
    }
    g_child_pid = 0;
}

static void test_wait4_signal_interrupt(void)
{
    TEST_START("waitpid without SA_RESTART returns EINTR after SIGALRM");
    install_alarm_handler(0);

    pid_t child = fork_sleep_child();
    if (child < 0) {
        return;
    }

    alarm(2);
    int status = 0;
    errno = 0;
    pid_t got = waitpid(child, &status, 0);
    CHECK(g_alarm_fired == 1, "SIGALRM handler fired while waitpid blocked");
    CHECK(got == -1 && errno == EINTR, "waitpid returns -1/EINTR without SA_RESTART");
    reap_after_interrupt(child);
    alarm(0);

    printf("WAIT4_SIGNAL_INTERRUPT_PASSED\n");
}

static void test_wait4_sarestart(void)
{
    TEST_START("waitpid with SA_RESTART resumes after SIGALRM handler");
    install_alarm_handler(SA_RESTART);

    pid_t child = fork_sleep_child();
    if (child < 0) {
        return;
    }

    alarm(2);
    int status = 0;
    errno = 0;
    pid_t got = waitpid(child, &status, 0);
    CHECK(g_alarm_fired == 1, "SIGALRM handler fired during restartable waitpid");
    CHECK_RET(got, child, "waitpid returns child pid with SA_RESTART");
    CHECK(WIFSIGNALED(status), "SA_RESTART child terminated by signal");
    if (WIFSIGNALED(status)) {
        CHECK(WTERMSIG(status) == SIGKILL, "SA_RESTART child terminated by SIGKILL");
    }
    g_child_pid = 0;
    alarm(0);

    printf("WAIT4_SARESTART_PASSED\n");
}

static void test_wait4_wnohang(void)
{
    TEST_START("waitpid WNOHANG is not spuriously interrupted");
    install_alarm_handler(0);

    pid_t child = fork_sleep_child();
    if (child < 0) {
        return;
    }

    alarm(2);
    int status = 0;
    int saw_running = 0;
    pid_t got = -1;
    for (int i = 0; i < 100; ++i) {
        errno = 0;
        got = waitpid(child, &status, WNOHANG);
        if (got == 0) {
            saw_running = 1;
        } else if (got == child) {
            break;
        } else if (got == -1 && errno == EINTR) {
            continue;
        } else {
            break;
        }

        struct timespec ts = {0, 100 * 1000 * 1000};
        nanosleep(&ts, NULL);
    }

    CHECK(saw_running == 1, "WNOHANG observed running child before alarm");
    CHECK(g_alarm_fired == 1, "SIGALRM handler fired during WNOHANG polling");
    CHECK_RET(got, child, "WNOHANG eventually reaps killed child");
    CHECK(WIFSIGNALED(status), "WNOHANG child terminated by signal");
    if (WIFSIGNALED(status)) {
        CHECK(WTERMSIG(status) == SIGKILL, "WNOHANG child terminated by SIGKILL");
    }
    g_child_pid = 0;
    alarm(0);

    printf("WAIT4_WNOHANG_PASSED\n");
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);

    test_wait4_signal_interrupt();
    test_wait4_sarestart();
    test_wait4_wnohang();

    CHECK(__fail == 0, "all wait4 signal interrupt subtests passed");
    if (__fail == 0) {
        printf("WAIT4_SIGNAL_INTERRUPT_ALL_PASSED\n");
    }
    TEST_DONE();
}
