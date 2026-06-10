#define _POSIX_C_SOURCE 200809L

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#ifndef PR_SET_CHILD_SUBREAPER
#define PR_SET_CHILD_SUBREAPER 36
#endif

#ifndef PR_GET_CHILD_SUBREAPER
#define PR_GET_CHILD_SUBREAPER 37
#endif

#define CHECK(expr, msg) do { \
    if (!(expr)) { \
        printf("  FAIL | %s:%d | %s\n", __FILE__, __LINE__, msg); \
        exit(1); \
    } \
    printf("  PASS | %s:%d | %s\n", __FILE__, __LINE__, msg); \
} while (0)

static int read_child_subreaper(void)
{
    int value = -1;
    int ret = prctl(PR_GET_CHILD_SUBREAPER, (unsigned long)&value, 0, 0, 0);
    CHECK(ret == 0, "PR_GET_CHILD_SUBREAPER returns 0");
    return value;
}

static void short_sleep(void)
{
    struct timespec ts = {
        .tv_sec = 0,
        .tv_nsec = 10 * 1000 * 1000,
    };
    nanosleep(&ts, NULL);
}

static void write_all_or_exit(int fd, const char *buf, size_t len)
{
    size_t written = 0;
    while (written < len) {
        ssize_t ret = write(fd, buf + written, len - written);
        if (ret < 0) {
            _exit(30);
        }
        written += (size_t)ret;
    }
}

static void read_line_or_fail(int fd, char *buf, size_t len)
{
    size_t pos = 0;
    while (pos + 1 < len) {
        char ch;
        ssize_t ret = read(fd, &ch, 1);
        if (ret < 0) {
            printf("  FAIL | %s:%d | read from child pipe does not fail\n",
                   __FILE__, __LINE__);
            exit(1);
        }
        if (ret == 0) {
            break;
        }
        buf[pos++] = ch;
        if (ch == '\n') {
            break;
        }
    }
    buf[pos] = '\0';
    CHECK(pos > 0, "child pipe contains a status line");
}

static void expect_child_exit(pid_t pid, int expected_status, const char *msg)
{
    int status;
    pid_t waited = waitpid(pid, &status, 0);
    CHECK(waited == pid, msg);
    CHECK(WIFEXITED(status), "child exits normally");
    CHECK(WEXITSTATUS(status) == expected_status, "child exit status matches");
}

static void test_set_get_and_fork_inheritance(void)
{
    CHECK(read_child_subreaper() == 0, "initial child subreaper flag is 0");

    CHECK(prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) == 0,
          "PR_SET_CHILD_SUBREAPER enables the flag");
    CHECK(read_child_subreaper() == 1, "enabled child subreaper flag reads back as 1");

    pid_t pid = fork();
    CHECK(pid >= 0, "fork child for inheritance check");
    if (pid == 0) {
        int value = -1;
        int ret = prctl(PR_GET_CHILD_SUBREAPER, (unsigned long)&value, 0, 0, 0);
        _exit(ret == 0 && value == 0 ? 0 : 10);
    }
    expect_child_exit(pid, 0, "child subreaper flag is not inherited by fork child");

    CHECK(prctl(PR_SET_CHILD_SUBREAPER, 0, 0, 0, 0) == 0,
          "PR_SET_CHILD_SUBREAPER disables the flag");
    CHECK(read_child_subreaper() == 0, "disabled child subreaper flag reads back as 0");
}

static void test_reparent_to_subreaper(void)
{
    int pipefd[2];
    CHECK(pipe(pipefd) == 0, "create pipe for orphan status");

    pid_t subreaper_pid = getpid();
    CHECK(prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) == 0,
          "enable child subreaper before reparent test");

    pid_t middle = fork();
    CHECK(middle >= 0, "fork middle child");
    if (middle == 0) {
        close(pipefd[0]);
        pid_t expected_reaper = getppid();
        pid_t leaf = fork();
        if (leaf < 0) {
            _exit(20);
        }
        if (leaf == 0) {
            pid_t old_parent = getppid();
            for (int i = 0; i < 200 && getppid() == old_parent; i++) {
                short_sleep();
            }

            pid_t observed_parent = getppid();
            char line[96];
            int len = snprintf(line, sizeof(line), "ppid=%ld expected=%ld\n",
                               (long)observed_parent, (long)expected_reaper);
            if (len <= 0 || (size_t)len >= sizeof(line)) {
                _exit(21);
            }
            write_all_or_exit(pipefd[1], line, (size_t)len);
            _exit(observed_parent == expected_reaper ? 0 : 22);
        }
        _exit(0);
    }

    close(pipefd[1]);

    char line[96];
    read_line_or_fail(pipefd[0], line, sizeof(line));
    close(pipefd[0]);

    long observed = -1;
    long expected = -1;
    CHECK(sscanf(line, "ppid=%ld expected=%ld", &observed, &expected) == 2,
          "parse orphan status line");
    CHECK(expected == (long)subreaper_pid, "middle child expected this process as reaper");
    CHECK(observed == (long)subreaper_pid, "orphan is reparented to child subreaper");

    expect_child_exit(middle, 0, "middle child exits successfully");

    int status;
    pid_t leaf = waitpid(-1, &status, 0);
    CHECK(leaf > 0, "subreaper can wait for adopted grandchild");
    CHECK(WIFEXITED(status), "adopted grandchild exits normally");
    CHECK(WEXITSTATUS(status) == 0, "adopted grandchild observed the subreaper parent");

    CHECK(prctl(PR_SET_CHILD_SUBREAPER, 0, 0, 0, 0) == 0,
          "disable child subreaper after reparent test");
}

int main(void)
{
    printf("=== test PR_SET/GET_CHILD_SUBREAPER ===\n");
    test_set_get_and_fork_inheritance();
    test_reparent_to_subreaper();
    printf("ALL PASSED\n");
    return 0;
}
