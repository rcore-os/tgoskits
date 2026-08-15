#define _GNU_SOURCE

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define TEST_DIRECTORY "/usr/bin/starry-test-suit"
#define DEFAULT_CASE_TIMEOUT_SECONDS 120
#define EXT4_INODE_UNIQUE_TIMEOUT_SECONDS 240
#define PAGECACHE_CAP_TIMEOUT_SECONDS 240
#define NAMESPACE_CLEANUP_TIMEOUT_SECONDS 30
#define RUNNER_TIMEOUT_STATUS 124
#define RUNNER_ERROR_STATUS 125
#define NAMESPACE_WAIT_NO_CHILD (-2)
#define SUPERVISOR_STACK_SIZE (64U * 1024U)
#define WAIT_POLL_NANOSECONDS (10L * 1000L * 1000L)

struct case_args {
    const char *path;
    const char *output_path;
};

static int exit_status_from_wait_status(int status);

static int timespec_reached(struct timespec now, struct timespec deadline)
{
    return now.tv_sec > deadline.tv_sec ||
           (now.tv_sec == deadline.tv_sec && now.tv_nsec >= deadline.tv_nsec);
}

static unsigned case_timeout_seconds(const char *name)
{
    if (strcmp(name, "test-ext4-inode-unique") == 0) {
        return EXT4_INODE_UNIQUE_TIMEOUT_SECONDS;
    }
    if (strcmp(name, "test-pagecache-cap") == 0) {
        return PAGECACHE_CAP_TIMEOUT_SECONDS;
    }
    return DEFAULT_CASE_TIMEOUT_SECONDS;
}

static int wait_for_namespace_init(pid_t namespace_init, int *status,
                                   unsigned timeout_seconds)
{
    struct timespec deadline;
    if (clock_gettime(CLOCK_MONOTONIC, &deadline) != 0) {
        perror("read system test timeout clock");
        return -1;
    }
    deadline.tv_sec += (time_t)timeout_seconds;

    for (;;) {
        pid_t reaped = waitpid(namespace_init, status, WNOHANG);
        if (reaped == namespace_init) {
            return 1;
        }
        if (reaped < 0) {
            if (errno == EINTR) {
                continue;
            }
            if (errno == ECHILD) {
                return NAMESPACE_WAIT_NO_CHILD;
            }
            perror("wait system test PID namespace");
            return -1;
        }

        struct timespec now;
        if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) {
            perror("read system test timeout clock");
            return -1;
        }
        if (timespec_reached(now, deadline)) {
            return 0;
        }

        const struct timespec pause = {
            .tv_sec = 0,
            .tv_nsec = WAIT_POLL_NANOSECONDS,
        };
        if (nanosleep(&pause, NULL) != 0 && errno != EINTR) {
            perror("sleep while waiting for system test PID namespace");
            return -1;
        }
    }
}

static int kill_and_reap_namespace_init(pid_t namespace_init)
{
    if (kill(namespace_init, SIGKILL) != 0 && errno != ESRCH) {
        perror("kill timed out system test PID namespace");
        return -1;
    }

    int status;
    int wait_result =
        wait_for_namespace_init(namespace_init, &status,
                                NAMESPACE_CLEANUP_TIMEOUT_SECONDS);
    if (wait_result == 1 || wait_result == NAMESPACE_WAIT_NO_CHILD) {
        return 0;
    }
    if (wait_result == 0) {
        fprintf(stderr,
                "STARRY_SYSTEM_TEST_CLEANUP_TIMEOUT: namespace_pid=%d timeout_s=%u\n",
                namespace_init, NAMESPACE_CLEANUP_TIMEOUT_SECONDS);
    }
    return -1;
}

static void redirect_output(const char *output_path)
{
    if (output_path == NULL) {
        return;
    }
    int fd = open(output_path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (fd < 0) {
        perror("open system test output");
        _exit(RUNNER_ERROR_STATUS);
    }
    if (dup2(fd, STDOUT_FILENO) < 0 || dup2(fd, STDERR_FILENO) < 0) {
        perror("redirect system test output");
        close(fd);
        _exit(RUNNER_ERROR_STATUS);
    }
    close(fd);
}

static int supervise_case_namespace(void *opaque)
{
    const struct case_args *args = opaque;
    if (setsid() < 0) {
        perror("setsid system test namespace");
        return RUNNER_ERROR_STATUS;
    }
    if (mount(NULL, "/", NULL, MS_REC | MS_PRIVATE, NULL) != 0) {
        perror("make system test mounts private");
        return RUNNER_ERROR_STATUS;
    }
    if (umount2("/proc", MNT_DETACH) != 0) {
        perror("detach inherited system test procfs");
        return RUNNER_ERROR_STATUS;
    }
    if (mount("proc", "/proc", "proc", MS_NOSUID | MS_NODEV | MS_NOEXEC, NULL) != 0) {
        perror("mount system test procfs");
        return RUNNER_ERROR_STATUS;
    }

    pid_t child = fork();
    if (child < 0) {
        perror("fork system test");
        return RUNNER_ERROR_STATUS;
    }
    if (child == 0) {
        redirect_output(args->output_path);
        execl(args->path, args->path, (char *)NULL);
        perror("exec system test");
        _exit(127);
    }

    int status;
    for (;;) {
        pid_t reaped = waitpid(child, &status, 0);
        if (reaped == child) {
            return exit_status_from_wait_status(status);
        }
        if (reaped < 0 && errno == EINTR) {
            continue;
        }
        perror("wait system test");
        return RUNNER_ERROR_STATUS;
    }
}

static int exit_status_from_wait_status(int status)
{
    if (WIFEXITED(status)) {
        return WEXITSTATUS(status);
    }
    if (WIFSIGNALED(status)) {
        return 128 + WTERMSIG(status);
    }
    return RUNNER_ERROR_STATUS;
}

static int run_isolated_case(const char *path, const char *output_path,
                             unsigned timeout_seconds)
{
    void *case_stack = malloc(SUPERVISOR_STACK_SIZE);
    if (case_stack == NULL) {
        perror("allocate system test namespace-init stack");
        return RUNNER_ERROR_STATUS;
    }
    struct case_args args = {
        .path = path,
        .output_path = output_path,
    };
    pid_t namespace_init = clone(supervise_case_namespace,
                                 (char *)case_stack + SUPERVISOR_STACK_SIZE,
                                 CLONE_NEWPID | CLONE_NEWNS | SIGCHLD, &args);
    if (namespace_init < 0) {
        perror("clone system test PID namespace");
        free(case_stack);
        return RUNNER_ERROR_STATUS;
    }

    int status;
    int wait_result =
        wait_for_namespace_init(namespace_init, &status, timeout_seconds);
    if (wait_result == NAMESPACE_WAIT_NO_CHILD) {
        free(case_stack);
        return RUNNER_ERROR_STATUS;
    }
    if (wait_result < 0) {
        (void)kill_and_reap_namespace_init(namespace_init);
        free(case_stack);
        return RUNNER_ERROR_STATUS;
    }
    if (wait_result == 0) {
        fprintf(stderr, "STARRY_SYSTEM_TEST_TIMEOUT: %s timeout_s=%u\n", path,
                timeout_seconds);
        int cleanup_result = kill_and_reap_namespace_init(namespace_init);
        free(case_stack);
        return cleanup_result == 0 ? RUNNER_TIMEOUT_STATUS : RUNNER_ERROR_STATUS;
    }
    free(case_stack);
    return exit_status_from_wait_status(status);
}

static int compare_names(const void *left, const void *right)
{
    const char *const *left_name = left;
    const char *const *right_name = right;
    return strcmp(*left_name, *right_name);
}

static void free_test_names(char **names, size_t count)
{
    for (size_t index = 0; index < count; ++index) {
        free(names[index]);
    }
    free(names);
}

static int collect_test_names(char ***collected_names, size_t *count)
{
    *collected_names = NULL;
    *count = 0;
    DIR *directory = opendir(TEST_DIRECTORY);
    if (directory == NULL) {
        perror("open system test directory");
        return -1;
    }

    char **names = NULL;
    size_t capacity = 0;
    *count = 0;
    for (;;) {
        errno = 0;
        struct dirent *entry = readdir(directory);
        if (entry == NULL) {
            if (errno != 0) {
                perror("read system test directory");
                goto fail;
            }
            break;
        }
        if (entry->d_name[0] == '.') {
            continue;
        }
        if (*count == capacity) {
            size_t next_capacity = capacity == 0 ? 16 : capacity * 2;
            if (next_capacity < capacity ||
                next_capacity > SIZE_MAX / sizeof(*names)) {
                errno = ENOMEM;
                perror("grow system test names");
                goto fail;
            }
            char **next_names = realloc(names, next_capacity * sizeof(*next_names));
            if (next_names == NULL) {
                perror("allocate system test names");
                goto fail;
            }
            names = next_names;
            capacity = next_capacity;
        }
        names[*count] = strdup(entry->d_name);
        if (names[*count] == NULL) {
            perror("copy system test name");
            goto fail;
        }
        *count += 1;
    }
    if (closedir(directory) != 0) {
        perror("close system test directory");
        free_test_names(names, *count);
        return -1;
    }
    if (*count > 1) {
        qsort(names, *count, sizeof(*names), compare_names);
    }
    *collected_names = names;
    return 0;

fail:
    {
        int saved_errno = errno;
        (void)closedir(directory);
        free_test_names(names, *count);
        errno = saved_errno;
        return -1;
    }
}

static struct timespec monotonic_now(void)
{
    struct timespec now = {0};
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) {
        perror("read system test monotonic clock");
    }
    return now;
}

static double elapsed_seconds(struct timespec start)
{
    struct timespec end = monotonic_now();
    time_t seconds = end.tv_sec - start.tv_sec;
    long nanoseconds = end.tv_nsec - start.tv_nsec;
    if (nanoseconds < 0) {
        seconds -= 1;
        nanoseconds += 1000000000L;
    }
    if (seconds < 0) {
        return 0.0;
    }
    return (double)seconds + (double)nanoseconds / 1000000000.0;
}

static void replay_output(const char *output_path)
{
    FILE *output = fopen(output_path, "r");
    if (output == NULL) {
        return;
    }
    char buffer[4096];
    size_t length;
    while ((length = fread(buffer, 1, sizeof(buffer), output)) != 0) {
        (void)fwrite(buffer, 1, length, stdout);
    }
    fclose(output);
}

int main(int argc, char **argv)
{
    int capture_failures = 0;
    if (argc == 2 && strcmp(argv[1], "--capture-failures") == 0) {
        capture_failures = 1;
    } else if (argc != 1) {
        fprintf(stderr, "usage: %s [--capture-failures]\n", argv[0]);
        return 2;
    }
    char output_path[] = "/tmp/starry-system-test-output.XXXXXX";
    if (capture_failures != 0) {
        int output_fd = mkstemp(output_path);
        if (output_fd < 0) {
            perror("create system test output");
            return RUNNER_ERROR_STATUS;
        }
        close(output_fd);
    }

    size_t name_count = 0;
    char **names = NULL;
    if (collect_test_names(&names, &name_count) != 0) {
        if (capture_failures != 0) {
            (void)unlink(output_path);
        }
        return RUNNER_ERROR_STATUS;
    }
    int total = 0;
    int passed = 0;
    int failed = 0;
    struct timespec suite_start = monotonic_now();

    for (size_t index = 0; index < name_count; ++index) {
        char path[512];
        int path_length = snprintf(path, sizeof(path), "%s/%s", TEST_DIRECTORY,
                                   names[index]);
        if (path_length < 0 || (size_t)path_length >= sizeof(path)) {
            fprintf(stderr, "system test path is too long: %s\n", names[index]);
            failed += 1;
            free(names[index]);
            continue;
        }

        struct stat metadata;
        if (stat(path, &metadata) != 0 || !S_ISREG(metadata.st_mode) ||
            access(path, X_OK) != 0) {
            free(names[index]);
            continue;
        }

        total += 1;
        struct timespec start = monotonic_now();
        printf("STARRY_SYSTEM_TEST_BEGIN: %s\n", path);
        fflush(stdout);
        unsigned timeout_seconds = case_timeout_seconds(names[index]);
        int exit_status = run_isolated_case(
            path, capture_failures != 0 ? output_path : NULL, timeout_seconds);
        double elapsed = elapsed_seconds(start);

        if (capture_failures != 0 && exit_status != 0) {
            replay_output(output_path);
        }
        if (exit_status == 0) {
            passed += 1;
            printf("STARRY_SYSTEM_TEST_PASSED: %s elapsed_s=%.3f\n", path,
                   elapsed);
        } else {
            failed += 1;
            printf("STARRY_SYSTEM_TEST_FAILED: %s status=%d elapsed_s=%.3f\n",
                   path, exit_status, elapsed);
        }
        fflush(stdout);
        free(names[index]);
        if (exit_status == RUNNER_ERROR_STATUS) {
            for (size_t remaining = index + 1; remaining < name_count;
                 ++remaining) {
                free(names[remaining]);
            }
            break;
        }
    }
    free(names);
    if (capture_failures != 0) {
        (void)unlink(output_path);
    }

    printf("STARRY_SYSTEM_TEST_SUMMARY: total=%d passed=%d failed=%d elapsed_s=%.3f\n",
           total, passed, failed, elapsed_seconds(suite_start));
    if (total == 0) {
        printf("STARRY_GROUPED_TEST_FAILED: no system tests found\n");
        return 1;
    }
    if (failed != 0) {
        printf("STARRY_GROUPED_TEST_FAILED: one or more system tests failed\n");
        return 1;
    }
    printf("STARRY_GROUPED_TESTS_PASSED\n");
    return 0;
}
