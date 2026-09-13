#define _GNU_SOURCE

/*
 * Regression for the conventional Linux /dev descriptor symlinks documented
 * by proc_pid_fd(5): /dev/fd points to /proc/self/fd, while the standard
 * stream aliases point to descriptors 0, 1, and 2 below that directory.
 */
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <spawn.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

static int failures;

static void check_symlink(const char *path, const char *expected)
{
    char target[64];
    ssize_t length = readlink(path, target, sizeof(target) - 1);

    if (length >= 0)
        target[length] = '\0';
    if (length >= 0 && strcmp(target, expected) == 0) {
        printf("PASS: %s -> %s\n", path, expected);
        return;
    }

    fprintf(stderr, "FAIL: %s target=%s expected=%s errno=%d (%s)\n", path,
            length >= 0 ? target : "<unavailable>", expected, errno,
            strerror(errno));
    failures++;
}

static void check_dynamic_descriptor_path(void)
{
    int pipe_fds[2];
    char path[64];

    if (pipe(pipe_fds) != 0) {
        fprintf(stderr, "FAIL: pipe: errno=%d (%s)\n", errno,
                strerror(errno));
        failures++;
        return;
    }

    snprintf(path, sizeof(path), "/dev/fd/%d", pipe_fds[0]);
    int duplicate = open(path, O_RDONLY | O_NONBLOCK | O_CLOEXEC);
    if (duplicate >= 0) {
        printf("PASS: dynamic descriptor path %s\n", path);
        close(duplicate);
    } else {
        fprintf(stderr, "FAIL: open %s: errno=%d (%s)\n", path, errno,
                strerror(errno));
        failures++;
    }

    close(pipe_fds[0]);
    close(pipe_fds[1]);
}

static void check_spawn_through_path_fd(void)
{
    int executable_fd = open("/proc/self/exe", O_PATH | O_CLOEXEC);
    char path[64];
    pid_t child = -1;
    char *const argv[] = {"bugfix-dev-fd-symlinks", "--executor-child", NULL};

    if (executable_fd < 0) {
        fprintf(stderr, "FAIL: open executable O_PATH: errno=%d (%s)\n",
                errno, strerror(errno));
        failures++;
        return;
    }

    snprintf(path, sizeof(path), "/proc/self/fd/%d", executable_fd);
    int result = posix_spawn(&child, path, NULL, NULL, argv, environ);
    if (result != 0) {
        fprintf(stderr, "FAIL: posix_spawn %s: error=%d (%s)\n", path,
                result, strerror(result));
        failures++;
        close(executable_fd);
        return;
    }

    int status = 0;
    if (waitpid(child, &status, 0) == child && WIFEXITED(status) &&
        WEXITSTATUS(status) == 0) {
        printf("PASS: posix_spawn executable through %s\n", path);
    } else {
        fprintf(stderr, "FAIL: spawned executable status=%d errno=%d (%s)\n",
                status, errno, strerror(errno));
        failures++;
    }
    close(executable_fd);
}

static int copy_self(const char *destination)
{
    int source = open("/proc/self/exe", O_RDONLY | O_CLOEXEC);
    int output = open(destination, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC,
                      0755);
    char buffer[4096];
    ssize_t length;

    if (source < 0 || output < 0)
        goto fail;
    while ((length = read(source, buffer, sizeof(buffer))) > 0) {
        ssize_t written = 0;
        while (written < length) {
            ssize_t result = write(output, buffer + written, length - written);
            if (result <= 0)
                goto fail;
            written += result;
        }
    }
    if (length < 0 || fchmod(output, 0755) != 0)
        goto fail;
    close(output);
    close(source);
    return 0;

fail:
    if (output >= 0)
        close(output);
    if (source >= 0)
        close(source);
    return -1;
}

static void check_spawn_deleted_executable_through_path_fd(void)
{
    const char *temporary = "/tmp/starry-deleted-executor";
    char path[64];
    pid_t child = -1;
    char *const argv[] = {"bugfix-dev-fd-symlinks", "--executor-child", NULL};

    unlink(temporary);
    if (copy_self(temporary) != 0) {
        fprintf(stderr, "FAIL: copy temporary executor: errno=%d (%s)\n",
                errno, strerror(errno));
        failures++;
        return;
    }

    int executable_fd = open(temporary, O_PATH | O_CLOEXEC);
    if (executable_fd < 0 || unlink(temporary) != 0) {
        fprintf(stderr, "FAIL: pin and unlink executor: errno=%d (%s)\n",
                errno, strerror(errno));
        failures++;
        if (executable_fd >= 0)
            close(executable_fd);
        return;
    }

    snprintf(path, sizeof(path), "/proc/self/fd/%d", executable_fd);
    int result = posix_spawn(&child, path, NULL, NULL, argv, environ);
    int status = 0;
    if (result == 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) &&
        WEXITSTATUS(status) == 0) {
        printf("PASS: spawn deleted executable through %s\n", path);
    } else {
        fprintf(stderr,
                "FAIL: spawn deleted executable result=%d status=%d error=%s\n",
                result, status, strerror(result));
        failures++;
    }
    close(executable_fd);
}

int main(int argc, char **argv)
{
    if (argc == 2 && strcmp(argv[1], "--executor-child") == 0) {
        puts("STARRY_PROC_FD_EXECUTOR_CHILD_PASSED");
        return 0;
    }

    check_symlink("/dev/fd", "/proc/self/fd");
    check_symlink("/dev/stdin", "/proc/self/fd/0");
    check_symlink("/dev/stdout", "/proc/self/fd/1");
    check_symlink("/dev/stderr", "/proc/self/fd/2");
    check_dynamic_descriptor_path();
    check_spawn_through_path_fd();
    check_spawn_deleted_executable_through_path_fd();

    if (failures != 0) {
        fprintf(stderr, "STARRY_DEV_FD_SYMLINKS_FAILED: %d checks\n", failures);
        return 1;
    }

    puts("STARRY_DEV_FD_SYMLINKS_PASSED");
    return 0;
}
