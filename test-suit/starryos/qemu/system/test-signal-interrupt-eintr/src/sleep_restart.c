#define _GNU_SOURCE

#include <errno.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static volatile sig_atomic_t sleep_signal_seen;

static void sleep_signal_handler(int signo)
{
    sleep_signal_seen = signo == SIGUSR1;
}

static int sleep_child(int ready_fd, clockid_t clock_id, int flags)
{
    struct sigaction action = {.sa_handler = sleep_signal_handler, .sa_flags = SA_RESTART};
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGUSR1, &action, NULL) != 0)
        return 1;
    sigset_t unblocked;
    sigemptyset(&unblocked);
    sigaddset(&unblocked, SIGUSR1);
    if (sigprocmask(SIG_UNBLOCK, &unblocked, NULL) != 0)
        return 1;
    sleep_signal_seen = 0;

    /* Prepare the request before readiness. After the byte is published,
     * clock_nanosleep is the child's only potentially blocking operation. */
    struct timespec request;
    if (syscall(SYS_clock_gettime, clock_id, &request) != 0)
        return 1;
    if (flags == TIMER_ABSTIME)
        request.tv_sec += 10;
    else
        request = (struct timespec){.tv_sec = 10};
    const struct timespec sentinel = {.tv_sec = 123, .tv_nsec = 456};
    struct timespec remaining = sentinel;
    if (write(ready_fd, "R", 1) != 1)
        return 1;
    close(ready_fd);

    errno = 0;
    long result = syscall(SYS_clock_nanosleep, clock_id, flags, &request, &remaining);
    int error = errno;
    if (result != -1 || error != EINTR || !sleep_signal_seen) {
        fprintf(stderr, "FAIL: clock_nanosleep clock=%d flags=%d result=%ld errno=%d signal=%d\n",
                (int)clock_id, flags, result, error, (int)sleep_signal_seen);
        return 1;
    }
    /* Relative sleeps must supply a normalized remainder. Its accuracy is
     * covered by LTP nanosleep02; timer slack can exceed the requested time. */
    if (flags == TIMER_ABSTIME) {
        if (remaining.tv_sec != sentinel.tv_sec || remaining.tv_nsec != sentinel.tv_nsec) {
            fprintf(stderr, "FAIL: absolute sleep modified remaining time\n");
            return 1;
        }
    } else if (remaining.tv_sec < 0 ||
               remaining.tv_nsec < 0 || remaining.tv_nsec >= 1000000000L ||
               (remaining.tv_sec == sentinel.tv_sec &&
                remaining.tv_nsec == sentinel.tv_nsec) ||
               (remaining.tv_sec == 0 && remaining.tv_nsec == 0)) {
        fprintf(stderr, "FAIL: relative sleep remaining=%lld.%09ld\n",
                (long long)remaining.tv_sec, remaining.tv_nsec);
        return 1;
    }
    return 0;
}

static int wait_for_sleep(pid_t child)
{
    char path[64];
    snprintf(path, sizeof(path), "/proc/%ld/status", (long)child);
    struct timespec started;
    if (clock_gettime(CLOCK_MONOTONIC, &started) != 0)
        return 1;
    for (;;) {
        FILE *file = fopen(path, "r");
        if (!file)
            return 1;
        char line[128], state = 0;
        while (fgets(line, sizeof(line), file)) {
            if (sscanf(line, "State: %c", &state) == 1)
                break;
        }
        fclose(file);
        if (state == 'S')
            return 0;
        if (state == 'Z' || state == 'X')
            return 1;
        struct timespec now;
        if (clock_gettime(CLOCK_MONOTONIC, &now) != 0 ||
            now.tv_sec - started.tv_sec >= 5)
            return 1;
        sched_yield();
    }
}

static int run_sleep_case(clockid_t clock_id, int flags)
{
    int ready_pipe[2];
    if (pipe(ready_pipe) != 0)
        return 1;
    pid_t child = fork();
    if (child < 0) {
        close(ready_pipe[0]);
        close(ready_pipe[1]);
        return 1;
    }
    if (child == 0) {
        close(ready_pipe[0]);
        _exit(sleep_child(ready_pipe[1], clock_id, flags));
    }
    close(ready_pipe[1]);
    char ready = 0;
    ssize_t count;
    do {
        count = read(ready_pipe[0], &ready, 1);
    } while (count < 0 && errno == EINTR);
    close(ready_pipe[0]);

    /* Observe the published blocking state instead of guessing a delay
     * between readiness and entry into the sleep syscall. */
    int failed = count != 1 || ready != 'R' || wait_for_sleep(child) != 0;
    if (failed || kill(child, SIGUSR1) != 0) {
        fprintf(stderr, "FAIL: sleep child did not reach an interruptible wait\n");
        kill(child, SIGKILL);
        failed = 1;
    }
    int status = 0;
    pid_t reaped;
    do {
        reaped = waitpid(child, &status, 0);
    } while (reaped < 0 && errno == EINTR);
    if (failed || reaped != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0)
        return 1;
    printf("PASS: clock_nanosleep clock=%d flags=%d preserves EINTR with SA_RESTART\n",
           (int)clock_id, flags);
    return 0;
}

int test_sleep_signal_restart(void)
{
    for (unsigned int i = 0; i < 2; ++i) {
        clockid_t clock_id = i == 0 ? CLOCK_MONOTONIC : CLOCK_REALTIME;
        if (run_sleep_case(clock_id, 0) || run_sleep_case(clock_id, TIMER_ABSTIME))
            return 1;
    }
    return 0;
}
