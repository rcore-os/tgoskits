#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int passed;
static int failed;

static void note_pass(const char *name)
{
    printf("PASS: %s\n", name);
    passed++;
}

static void note_fail(const char *name, const char *detail)
{
    printf("FAIL: %s: %s\n", name, detail);
    failed++;
}

static void sleep_briefly(void)
{
    struct timespec ts = {0, 100000000};
    nanosleep(&ts, NULL);
}

static int run_same_uid_nonroot_kill_test(void)
{
    if (setuid(1000) != 0) {
        note_fail("drop uid for same-uid zombie kill", strerror(errno));
        return 1;
    }
    if (geteuid() != 1000) {
        note_fail("drop uid for same-uid zombie kill", "euid did not become 1000");
        return 1;
    }

    pid_t child = fork();
    if (child < 0) {
        note_fail("fork nonroot zombie child", strerror(errno));
        return 1;
    }
    if (child == 0) {
        _exit(0);
    }

    sleep_briefly();

    errno = 0;
    int ret = kill(child, 0);
    if (ret == 0) {
        note_pass("nonroot same-uid kill(pid, 0) sees unreaped zombie child");
    } else {
        char detail[160];
        snprintf(detail, sizeof(detail),
                 "ret=%d errno=%d (%s), expected success",
                 ret, errno, strerror(errno));
        note_fail("nonroot same-uid kill zombie child probe", detail);
    }

    errno = 0;
    ret = kill(child, SIGKILL);
    if (ret == 0) {
        note_pass("nonroot same-uid kill(pid, SIGKILL) accepts unreaped zombie child");
    } else {
        char detail[160];
        snprintf(detail, sizeof(detail),
                 "ret=%d errno=%d (%s), expected success",
                 ret, errno, strerror(errno));
        note_fail("nonroot same-uid kill zombie child SIGKILL", detail);
    }

    int status = 0;
    pid_t waited = waitpid(child, &status, 0);
    if (waited == child && WIFEXITED(status) && WEXITSTATUS(status) == 0) {
        note_pass("waitpid reaps nonroot zombie child");
    } else {
        char detail[160];
        snprintf(detail, sizeof(detail),
                 "waited=%d errno=%d status=0x%x",
                 waited, errno, status);
        note_fail("waitpid nonroot zombie child", detail);
    }

    return 0;
}

int main(void)
{
    printf("=== bug-zombie-process-queries ===\n");

    int sync_pipe[2];
    if (pipe(sync_pipe) != 0) {
        note_fail("pipe", strerror(errno));
        return 1;
    }

    pid_t child = fork();
    if (child < 0) {
        note_fail("fork", strerror(errno));
        return 1;
    }

    if (child == 0) {
        close(sync_pipe[0]);
        if (setpgid(0, 0) != 0) {
            _exit(10);
        }
        char ready = 'R';
        if (write(sync_pipe[1], &ready, 1) != 1) {
            _exit(11);
        }
        close(sync_pipe[1]);
        _exit(0);
    }

    close(sync_pipe[1]);
    char ready = 0;
    if (read(sync_pipe[0], &ready, 1) != 1 || ready != 'R') {
        note_fail("child setpgid sync", strerror(errno));
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
        return 1;
    }
    close(sync_pipe[0]);

    sleep_briefly();

    pid_t zombie_pgid = getpgid(child);
    if (zombie_pgid == child) {
        note_pass("getpgid sees unreaped zombie child");
    } else {
        char detail[160];
        snprintf(detail, sizeof(detail),
                 "getpgid returned %d errno=%d (%s), expected %d",
                 zombie_pgid, errno, strerror(errno), child);
        note_fail("getpgid zombie child", detail);
    }

    errno = 0;
    int ret = kill(child, 0);
    if (ret == 0) {
        note_pass("kill(pid, 0) sees unreaped zombie child");
    } else {
        char detail[160];
        snprintf(detail, sizeof(detail),
                 "ret=%d errno=%d (%s), expected success",
                 ret, errno, strerror(errno));
        note_fail("kill zombie child probe", detail);
    }

    errno = 0;
    ret = kill(child, SIGKILL);
    if (ret == 0) {
        note_pass("kill(pid, SIGKILL) accepts unreaped zombie child");
    } else {
        char detail[160];
        snprintf(detail, sizeof(detail),
                 "ret=%d errno=%d (%s), expected success",
                 ret, errno, strerror(errno));
        note_fail("kill zombie child SIGKILL", detail);
    }

    errno = 0;
    ret = kill(-child, 0);
    if (ret == 0) {
        note_pass("kill(-pgid, 0) sees zombie process group");
    } else {
        char detail[160];
        snprintf(detail, sizeof(detail),
                 "ret=%d errno=%d (%s), expected success",
                 ret, errno, strerror(errno));
        note_fail("kill zombie process group probe", detail);
    }

    int status = 0;
    pid_t waited = waitpid(child, &status, 0);
    if (waited == child && WIFEXITED(status) && WEXITSTATUS(status) == 0) {
        note_pass("waitpid reaps zombie child");
    } else {
        char detail[160];
        snprintf(detail, sizeof(detail),
                 "waited=%d errno=%d status=0x%x",
                 waited, errno, status);
        note_fail("waitpid zombie child", detail);
    }

    errno = 0;
    ret = kill(child, 0);
    if (ret == -1 && errno == ESRCH) {
        note_pass("kill(pid, 0) returns ESRCH after waitpid");
    } else {
        char detail[160];
        snprintf(detail, sizeof(detail),
                 "ret=%d errno=%d (%s), expected -1/ESRCH",
                 ret, errno, strerror(errno));
        note_fail("kill reaped child probe", detail);
    }

    run_same_uid_nonroot_kill_test();

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("ALL TESTS PASSED\n");
        return 0;
    }
    printf("SOME TESTS FAILED\n");
    return 1;
}
