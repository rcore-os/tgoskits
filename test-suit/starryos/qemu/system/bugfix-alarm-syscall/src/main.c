#define _GNU_SOURCE

/*
 * Linux x86_64 exposes alarm(2) as a syscall sharing the process-wide
 * ITIMER_REAL state with setitimer(2). A pending subsecond timer must return
 * one rather than zero so callers cannot mistake it for a disarmed timer.
 */
#include <errno.h>
#include <signal.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <unistd.h>

#if defined(SYS_alarm)

static int fail(const char *operation)
{
    fputs("FAIL: ", stderr);
    fputs(operation, stderr);
    fputs(" errno=", stderr);
    fputs(strerror(errno), stderr);
    fputc('\n', stderr);
    puts("STARRY_GROUPED_TEST_FAILED: bugfix-alarm-syscall");
    return EXIT_FAILURE;
}

static int timer_is_disarmed(void)
{
    struct itimerval current = {0};
    if (getitimer(ITIMER_REAL, &current) < 0) {
        return 0;
    }
    return current.it_interval.tv_sec == 0 &&
           current.it_interval.tv_usec == 0 &&
           current.it_value.tv_sec == 0 &&
           current.it_value.tv_usec == 0;
}

static int timer_is_single_shot_near(unsigned int seconds)
{
    struct itimerval current = {0};
    if (getitimer(ITIMER_REAL, &current) < 0) {
        return 0;
    }
    if (current.it_interval.tv_sec != 0 || current.it_interval.tv_usec != 0) {
        return 0;
    }

    int64_t remaining_us =
        (int64_t)current.it_value.tv_sec * 1000000 + current.it_value.tv_usec;
    int64_t requested_us = (int64_t)seconds * 1000000;
    return remaining_us > 0 && remaining_us <= requested_us;
}

struct sibling_observation {
    int getitimer_result;
    struct itimerval timer;
    long canceled_seconds;
};

static void *inspect_and_cancel_alarm(void *argument)
{
    struct sibling_observation *observation = argument;
    observation->getitimer_result =
        getitimer(ITIMER_REAL, &observation->timer);
    observation->canceled_seconds = syscall(SYS_alarm, 0U);
    return NULL;
}

static int timer_is_armed(const struct itimerval *timer)
{
    return timer->it_value.tv_sec != 0 || timer->it_value.tv_usec != 0;
}

static volatile sig_atomic_t alarm_delivered;

static void record_alarm(int signal_number)
{
    (void)signal_number;
    alarm_delivered = 1;
}

static void *arm_alarm_and_exit(void *argument)
{
    long *previous = argument;
    *previous = syscall(SYS_alarm, 1U);
    return NULL;
}

int main(void)
{
    struct itimerval timer = {0};
    if (setitimer(ITIMER_REAL, &timer, NULL) < 0) {
        return fail("reset ITIMER_REAL");
    }

    errno = 0;
    long previous = syscall(SYS_alarm, 5U);
    if (previous != 0) {
        errno = previous < 0 ? errno : EPROTO;
        return fail("arm alarm from disarmed state");
    }
    if (!timer_is_single_shot_near(5U)) {
        errno = EPROTO;
        return fail("inspect armed alarm");
    }

    previous = syscall(SYS_alarm, 0U);
    if (previous < 1 || previous > 5) {
        errno = EPROTO;
        return fail("cancel alarm and return remaining seconds");
    }
    if (!timer_is_disarmed()) {
        errno = EPROTO;
        return fail("verify alarm cancellation");
    }

    previous = syscall(SYS_alarm, 5U);
    if (previous != 0) {
        errno = previous < 0 ? errno : EPROTO;
        return fail("arm alarm for sibling thread");
    }

    struct sibling_observation observation = {0};
    pthread_t sibling;
    int thread_error =
        pthread_create(&sibling, NULL, inspect_and_cancel_alarm, &observation);
    if (thread_error != 0) {
        errno = thread_error;
        return fail("create sibling alarm observer");
    }
    thread_error = pthread_join(sibling, NULL);
    if (thread_error != 0) {
        errno = thread_error;
        return fail("join sibling alarm observer");
    }
    if (observation.getitimer_result != 0 ||
        !timer_is_armed(&observation.timer)) {
        errno = EPROTO;
        return fail("observe process alarm from sibling thread");
    }
    if (observation.canceled_seconds < 1 ||
        observation.canceled_seconds > 5 || !timer_is_disarmed()) {
        errno = EPROTO;
        return fail("cancel process alarm from sibling thread");
    }

    struct sigaction action = {
        .sa_handler = record_alarm,
    };
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGALRM, &action, NULL) < 0) {
        return fail("install process alarm handler");
    }

    long worker_previous = -1;
    thread_error =
        pthread_create(&sibling, NULL, arm_alarm_and_exit, &worker_previous);
    if (thread_error != 0) {
        errno = thread_error;
        return fail("create alarm owner thread");
    }
    thread_error = pthread_join(sibling, NULL);
    if (thread_error != 0) {
        errno = thread_error;
        return fail("join alarm owner thread");
    }
    if (worker_previous != 0) {
        errno = EPROTO;
        return fail("arm process alarm from sibling thread");
    }
    for (int attempt = 0; attempt < 30 && !alarm_delivered; attempt++) {
        usleep(100000);
    }
    if (!alarm_delivered || !timer_is_disarmed()) {
        errno = EPROTO;
        return fail("deliver sibling alarm to process");
    }

    timer.it_value.tv_sec = 1;
    timer.it_value.tv_usec = 250000;
    if (setitimer(ITIMER_REAL, &timer, NULL) < 0) {
        return fail("arm one-second fractional ITIMER_REAL");
    }

    previous = syscall(SYS_alarm, 0U);
    if (previous != 2) {
        errno = EPROTO;
        return fail("round one-second fractional remainder upward");
    }
    if (!timer_is_disarmed()) {
        errno = EPROTO;
        return fail("verify one-second fractional alarm cancellation");
    }

    timer.it_value.tv_sec = 0;
    timer.it_value.tv_usec = 250000;
    if (setitimer(ITIMER_REAL, &timer, NULL) < 0) {
        return fail("arm fractional ITIMER_REAL");
    }

    previous = syscall(SYS_alarm, 7U);
    if (previous != 1) {
        errno = EPROTO;
        return fail("preserve a pending subsecond remainder");
    }
    if (!timer_is_single_shot_near(7U)) {
        errno = EPROTO;
        return fail("verify alarm replaces ITIMER_REAL");
    }

    previous = syscall(SYS_alarm, 0U);
    if (previous < 1 || previous > 7 || !timer_is_disarmed()) {
        errno = EPROTO;
        return fail("final alarm cleanup");
    }

    puts("STARRY_ALARM_SYSCALL_PASSED");
    puts("STARRY_GROUPED_TEST_PASSED: bugfix-alarm-syscall");
    return EXIT_SUCCESS;
}
#else
int main(void)
{
    puts("SKIP: SYS_alarm is unavailable on this architecture");
    puts("STARRY_GROUPED_TEST_PASSED: bugfix-alarm-syscall");
    return EXIT_SUCCESS;
}
#endif
