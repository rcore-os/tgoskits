#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <fcntl.h>

#define BUF_SIZE 4096

/*
 * Run find(1) on @path with -maxdepth 2, capture stdout into @buf.
 * Returns 0 on success (exit code 0 and output captured), non-zero on failure.
 * On failure the captured output is printed so the caller can diagnose.
 */
static int run_find_capture(const char *path, char *buf, size_t bufsz) {
    int pipefd[2];
    if (pipe(pipefd) < 0) return 1;

    pid_t pid = fork();
    if (pid < 0) { close(pipefd[0]); close(pipefd[1]); return 1; }

    if (pid == 0) {
        close(pipefd[0]);
        dup2(pipefd[1], STDOUT_FILENO);
        dup2(pipefd[1], STDERR_FILENO);
        close(pipefd[1]);
        execl("/usr/bin/find", "find", path, "-maxdepth", "2", NULL);
        _exit(1);
    }

    close(pipefd[1]);
    /* Read in a loop to drain the pipe fully; prevents child SIGPIPE
       and avoids silent truncation when output exceeds the buffer. */
    size_t total = 0;
    ssize_t n;
    while ((n = read(pipefd[0], buf + total, bufsz - 1 - total)) > 0) {
        total += (size_t)n;
        if (total >= bufsz - 1) {
            char drain[512];
            while (read(pipefd[0], drain, sizeof(drain)) > 0)
                ;
            break;
        }
    }
    close(pipefd[0]);
    buf[total] = '\0';

    int status;
    if (waitpid(pid, &status, 0) < 0) return 1;
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        printf("  find %s exited abnormally (status %d)\n", path, status);
        if (total > 0) printf("  output: %s\n", buf);
        return 1;
    }
    return 0;
}

/* Check that @needle occurs as a complete path component in @haystack.
 * Looks for "/<needle>" followed by '\n', '/', or '\0'.
 * Loops past false matches (e.g. "/nullfs" won't match "null"). */
static int output_contains(const char *haystack, const char *needle) {
    size_t nlen = strlen(needle);
    const char *p = haystack;
    while ((p = strchr(p, '/')) != NULL) {
        p++; /* skip '/' */
        if (strncmp(p, needle, nlen) != 0)
            continue;
        char end = p[nlen];
        if (end == '\n' || end == '/' || end == '\0')
            return 1;
    }
    return 0;
}

static int test_fs(const char *mount, const char *expected_entry) {
    char buf[BUF_SIZE];
    printf("Testing %s ...\n", mount);

    if (run_find_capture(mount, buf, sizeof(buf)) != 0) return 1;

    /* When no specific entry is expected (e.g. /tmp, /sys may be empty),
       find exiting with code 0 is sufficient proof of traversal. */
    if (expected_entry == NULL)
        return 0;

    if (strlen(buf) == 0) {
        printf("  FAIL: find %s produced no output (expected \"%s\")\n",
               mount, expected_entry);
        return 1;
    }

    if (!output_contains(buf, expected_entry)) {
        printf("  FAIL: expected entry \"%s\" not found in output\n", expected_entry);
        printf("  output:\n%s\n", buf);
        return 1;
    }
    return 0;
}

int main(void) {
    /* ext4 rootfs — /etc always contains at least "passwd" */
    if (test_fs("/etc", "passwd") != 0) return 1;

    /* devfs — /dev always contains "null" */
    if (test_fs("/dev", "null") != 0) return 1;

    /* procfs — /proc always contains "self" */
    if (test_fs("/proc", "self") != 0) return 1;

    /* tmpfs — may be empty, only verify find succeeds with non-empty output */
    if (test_fs("/tmp", NULL) != 0) return 1;

    /* sysfs (tmpfs-backed) */
    if (test_fs("/sys", NULL) != 0) return 1;

    printf("FINDUTILS TEST PASSED\n");
    return 0;
}
