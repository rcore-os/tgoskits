#define _GNU_SOURCE

#include <errno.h>
#include <inttypes.h>
#include <limits.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

enum {
    CALIBRATION_SLEEP_NS = 25 * 1000 * 1000,
    PARENT_SLEEP_NS = 100 * 1000 * 1000,
    PARENT_WAKE_LIMIT_NS = 2 * 1000 * 1000 * 1000,
    CHILD_SPIN_MULTIPLIER = 120,
};

static const uint64_t MIN_CHILD_SPIN_CYCLES = UINT64_C(8) * 1000 * 1000 * 1000;

static uint64_t read_tsc(void)
{
    uint32_t low;
    uint32_t high;

    __asm__ volatile("lfence\n\trdtsc" : "=a"(low), "=d"(high) :: "memory");
    return ((uint64_t)high << 32) | low;
}

static uint64_t monotonic_time_ns(void)
{
    struct timespec now;

    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) {
        return 0;
    }
    return (uint64_t)now.tv_sec * UINT64_C(1000000000) + (uint64_t)now.tv_nsec;
}

static int sleep_ns(long nanoseconds)
{
    const struct timespec requested = {
        .tv_sec = nanoseconds / 1000000000L,
        .tv_nsec = nanoseconds % 1000000000L,
    };
    struct timespec remaining = requested;

    while (nanosleep(&remaining, &remaining) != 0) {
        if (errno != EINTR) {
            return -1;
        }
    }
    return 0;
}

static uint64_t calibrate_child_spin_cycles(void)
{
    const uint64_t started = read_tsc();

    if (sleep_ns(CALIBRATION_SLEEP_NS) != 0) {
        return 0;
    }

    const uint64_t elapsed_cycles = read_tsc() - started;
    if (elapsed_cycles == 0) {
        return 0;
    }
    if (elapsed_cycles > UINT64_MAX / CHILD_SPIN_MULTIPLIER) {
        return UINT64_MAX;
    }

    const uint64_t calibrated_cycles = elapsed_cycles * CHILD_SPIN_MULTIPLIER;
    return calibrated_cycles > MIN_CHILD_SPIN_CYCLES ? calibrated_cycles :
                                                        MIN_CHILD_SPIN_CYCLES;
}

static int read_child_ready(int ready_fd)
{
    char ready = '\0';

    for (;;) {
        const ssize_t read_len = read(ready_fd, &ready, sizeof(ready));
        if (read_len == 1) {
            return ready == 'R' ? 0 : -1;
        }
        if (read_len == -1 && errno == EINTR) {
            continue;
        }
        return -1;
    }
}

static void spin_in_user_mode(uint64_t spin_cycles)
{
    const uint64_t deadline = read_tsc() + spin_cycles;

    while ((int64_t)(read_tsc() - deadline) < 0) {
        __asm__ volatile("pause" ::: "memory");
    }
}

static int terminate_child(pid_t child)
{
    int status = 0;

    if (kill(child, SIGKILL) != 0 && errno != ESRCH) {
        return -1;
    }

    while (waitpid(child, &status, 0) != child) {
        if (errno != EINTR) {
            return -1;
        }
    }
    return 0;
}

static int verify_nanosleep_wakes_while_child_spins(uint64_t spin_cycles)
{
    int ready_pipe[2] = {-1, -1};
    int start_pipe[2] = {-1, -1};
    pid_t child = -1;
    int result = -1;

    if (pipe(ready_pipe) != 0) {
        perror("pipe");
        return -1;
    }
    if (pipe(start_pipe) != 0) {
        perror("pipe");
        goto out;
    }

    child = fork();
    if (child < 0) {
        perror("fork");
        goto out;
    }
    if (child == 0) {
        close(ready_pipe[0]);
        close(start_pipe[1]);
        if (write(ready_pipe[1], "R", 1) != 1) {
            _exit(1);
        }
        close(ready_pipe[1]);
        if (read_child_ready(start_pipe[0]) != 0) {
            _exit(1);
        }
        close(start_pipe[0]);
        spin_in_user_mode(spin_cycles);
        _exit(0);
    }

    close(ready_pipe[1]);
    ready_pipe[1] = -1;
    close(start_pipe[0]);
    start_pipe[0] = -1;
    if (read_child_ready(ready_pipe[0]) != 0) {
        fprintf(stderr, "FAIL: child did not enter its CPU-bound user loop\n");
        goto out;
    }
    close(ready_pipe[0]);
    ready_pipe[0] = -1;

    const uint64_t started = monotonic_time_ns();
    if (started == 0 || write(start_pipe[1], "R", 1) != 1 ||
        sleep_ns(PARENT_SLEEP_NS) != 0) {
        perror("nanosleep");
        goto out;
    }
    close(start_pipe[1]);
    start_pipe[1] = -1;
    const uint64_t finished = monotonic_time_ns();
    if (finished == 0 || finished < started) {
        fprintf(stderr, "FAIL: CLOCK_MONOTONIC did not provide a valid sleep duration\n");
        goto out;
    }

    const uint64_t elapsed_ns = finished - started;
    if (elapsed_ns > PARENT_WAKE_LIMIT_NS) {
        fprintf(stderr,
                "FAIL: nanosleep parent woke after %" PRIu64
                " ns while the same-CPU child was CPU-bound (limit=%d ns)\n",
                elapsed_ns, PARENT_WAKE_LIMIT_NS);
        goto out;
    }
    if (waitpid(child, NULL, WNOHANG) != 0) {
        fprintf(stderr,
                "FAIL: child stopped spinning before the parent woke; "
                "timer preemption was not exercised\n");
        goto out;
    }

    printf("PASS: nanosleep parent woke after %" PRIu64
           " ns while the same-CPU child was CPU-bound\n",
           elapsed_ns);
    result = 0;

out:
    if (ready_pipe[0] >= 0) {
        close(ready_pipe[0]);
    }
    if (ready_pipe[1] >= 0) {
        close(ready_pipe[1]);
    }
    if (start_pipe[0] >= 0) {
        close(start_pipe[0]);
    }
    if (start_pipe[1] >= 0) {
        close(start_pipe[1]);
    }
    if (child > 0 && terminate_child(child) != 0) {
        perror("terminate child");
        result = -1;
    }
    return result;
}

int main(void)
{
    const uint64_t spin_cycles = calibrate_child_spin_cycles();
    if (spin_cycles == 0) {
        fprintf(stderr, "FAIL: unable to calibrate a finite x86 TSC spin budget\n");
        printf("STARRY_TIMER_PREEMPTION_X86_FAILED\n");
        return 1;
    }

    if (verify_nanosleep_wakes_while_child_spins(spin_cycles) != 0) {
        printf("STARRY_TIMER_PREEMPTION_X86_FAILED\n");
        return 1;
    }

    printf("STARRY_TIMER_PREEMPTION_X86_PASSED\n");
    return 0;
}
