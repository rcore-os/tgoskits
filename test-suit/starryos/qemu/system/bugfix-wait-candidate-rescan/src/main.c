#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

enum wait_api {
    WAIT_API_WAITPID,
    WAIT_API_WAITID,
};

struct wait_result {
    pid_t pid;
    int status;
    int error;
};

struct waiter_args {
    enum wait_api api;
    int ready_fd;
    int result_fd;
};

static int failures;

static void fail_errno(const char *stage)
{
    printf("FAIL: %s: errno=%d (%s)\n", stage, errno, strerror(errno));
    failures++;
}

static int write_all(int fd, const void *buffer, size_t length)
{
    const char *cursor = buffer;

    while (length > 0) {
        ssize_t written = write(fd, cursor, length);
        if (written < 0) {
            if (errno == EINTR)
                continue;
            return -1;
        }
        cursor += written;
        length -= (size_t)written;
    }
    return 0;
}

static int read_all(int fd, void *buffer, size_t length)
{
    char *cursor = buffer;

    while (length > 0) {
        ssize_t received = read(fd, cursor, length);
        if (received < 0) {
            if (errno == EINTR)
                continue;
            return -1;
        }
        if (received == 0) {
            errno = EPIPE;
            return -1;
        }
        cursor += received;
        length -= (size_t)received;
    }
    return 0;
}

static void *waiter_main(void *opaque)
{
    const struct waiter_args *args = opaque;
    pid_t tid = (pid_t)syscall(SYS_gettid);
    struct wait_result result = {
        .pid = -1,
    };

    if (write_all(args->ready_fd, &tid, sizeof(tid)) != 0)
        return NULL;

    errno = 0;
    if (args->api == WAIT_API_WAITPID) {
        result.pid = waitpid(-1, &result.status, 0);
        result.error = result.pid < 0 ? errno : 0;
    } else {
        siginfo_t info;
        memset(&info, 0, sizeof(info));
        int ret = (int)syscall(SYS_waitid, P_ALL, 0, &info, WEXITED, NULL);
        result.pid = ret == 0 ? info.si_pid : -1;
        result.status = ret == 0 ? info.si_status : 0;
        result.error = ret < 0 ? errno : 0;
    }

    (void)write_all(args->result_fd, &result, sizeof(result));
    return NULL;
}

static int task_is_sleeping(pid_t tid)
{
    char path[64];
    int length = snprintf(path, sizeof(path), "/proc/self/task/%ld/status",
                          (long)tid);
    if (length < 0 || (size_t)length >= sizeof(path)) {
        errno = ENAMETOOLONG;
        return -1;
    }

    FILE *status = fopen(path, "r");
    if (status == NULL)
        return -1;

    char line[128];
    int sleeping = 0;
    while (fgets(line, sizeof(line), status) != NULL) {
        if (strncmp(line, "State:\tS", 8) == 0) {
            sleeping = 1;
            break;
        }
    }
    fclose(status);
    return sleeping;
}

static int wait_until_task_sleeps(pid_t tid)
{
    for (int attempt = 0; attempt < 500; ++attempt) {
        int sleeping = task_is_sleeping(tid);
        if (sleeping != 0)
            return sleeping;
        usleep(10000);
    }
    errno = ETIMEDOUT;
    return 0;
}

static int read_result_with_timeout(int fd, struct wait_result *result,
                                    int timeout_ms)
{
    struct pollfd pollfd = {
        .fd = fd,
        .events = POLLIN,
    };

    int ready;
    do {
        ready = poll(&pollfd, 1, timeout_ms);
    } while (ready < 0 && errno == EINTR);
    if (ready <= 0)
        return ready;
    return read_all(fd, result, sizeof(*result)) == 0 ? 1 : -1;
}

static pid_t spawn_blocked_child(int release_fd, int ready_fd)
{
    pid_t child = fork();
    if (child != 0)
        return child;

    char marker = 'R';
    if (write_all(ready_fd, &marker, 1) != 0)
        _exit(101);
    if (read(release_fd, &marker, 1) != 1)
        _exit(102);
    _exit(91);
}

static void reap_child(pid_t child)
{
    int status;
    pid_t waited;

    do {
        waited = waitpid(child, &status, 0);
    } while (waited < 0 && errno == EINTR);
}

static void exercise_rescan(enum wait_api api, const char *name)
{
    int sentinel_release[2];
    int sentinel_ready[2];
    int waiter_ready[2];
    int waiter_result[2];
    if (pipe(sentinel_release) != 0 || pipe(sentinel_ready) != 0 ||
        pipe(waiter_ready) != 0 || pipe(waiter_result) != 0) {
        fail_errno("create synchronization pipes");
        return;
    }

    pid_t sentinel = spawn_blocked_child(sentinel_release[0], sentinel_ready[1]);
    if (sentinel < 0) {
        fail_errno("fork sentinel child");
        return;
    }
    char marker;
    if (read_all(sentinel_ready[0], &marker, 1) != 0) {
        fail_errno("wait for sentinel child");
        return;
    }

    struct waiter_args args = {
        .api = api,
        .ready_fd = waiter_ready[1],
        .result_fd = waiter_result[1],
    };
    pthread_t waiter;
    int thread_error = pthread_create(&waiter, NULL, waiter_main, &args);
    if (thread_error != 0) {
        errno = thread_error;
        fail_errno("create waiter thread");
        return;
    }

    pid_t waiter_tid;
    if (read_all(waiter_ready[0], &waiter_tid, sizeof(waiter_tid)) != 0 ||
        wait_until_task_sleeps(waiter_tid) != 1) {
        fail_errno("observe waiter blocked in wait syscall");
        (void)write_all(sentinel_release[1], "X", 1);
        pthread_join(waiter, NULL);
        return;
    }

    pid_t candidate = fork();
    if (candidate < 0) {
        fail_errno("fork candidate child");
        (void)write_all(sentinel_release[1], "X", 1);
        pthread_join(waiter, NULL);
        return;
    }
    if (candidate == 0)
        _exit(42);

    struct wait_result result = {0};
    int result_ready = read_result_with_timeout(waiter_result[0], &result, 3000);
    if (result_ready == 1 && result.pid == candidate && result.error == 0 &&
        ((api == WAIT_API_WAITPID && WIFEXITED(result.status) &&
          WEXITSTATUS(result.status) == 42) ||
         (api == WAIT_API_WAITID && result.status == 42))) {
        printf("PASS: %s rescans children published after blocking\n", name);
    } else if (result_ready == 0) {
        printf("FAIL: %s kept the pre-block child snapshot\n", name);
        failures++;
    } else {
        printf("FAIL: %s returned pid=%ld status=%#x error=%d, expected pid=%ld\n",
               name, (long)result.pid, result.status, result.error,
               (long)candidate);
        failures++;
    }

    (void)write_all(sentinel_release[1], "X", 1);
    if (result_ready == 0)
        (void)read_result_with_timeout(waiter_result[0], &result, 3000);
    pthread_join(waiter, NULL);
    reap_child(candidate);
    reap_child(sentinel);
}

static void timeout_handler(int signo)
{
    (void)signo;
    static const char failure[] =
        "WAIT_CANDIDATE_RESCAN_FAILED stage=global-timeout\n";
    (void)write(STDOUT_FILENO, failure, sizeof(failure) - 1);
    _exit(1);
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    signal(SIGALRM, timeout_handler);
    alarm(30);

    exercise_rescan(WAIT_API_WAITPID, "waitpid");
    exercise_rescan(WAIT_API_WAITID, "waitid");

    alarm(0);
    if (failures != 0) {
        printf("WAIT_CANDIDATE_RESCAN_FAILED failures=%d\n", failures);
        return 1;
    }
    puts("WAIT_CANDIDATE_RESCAN_PASSED");
    return 0;
}
