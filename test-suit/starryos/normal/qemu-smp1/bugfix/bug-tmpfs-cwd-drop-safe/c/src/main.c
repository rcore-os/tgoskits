#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
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

static void remove_dir_if_exists(const char *path)
{
    if (rmdir(path) != 0 && errno != ENOENT) {
        printf("WARN: cleanup rmdir(%s): %s\n", path, strerror(errno));
    }
}

static void expect_child_cwd_drop_is_safe(void)
{
    const char *dir = "/tmp/bug-tmpfs-cwd-drop-safe";
    int ready_pipe[2] = {-1, -1};
    int release_pipe[2] = {-1, -1};

    remove_dir_if_exists(dir);
    if (mkdir(dir, 0700) != 0) {
        note_fail("mkdir tmpfs cwd dir", strerror(errno));
        return;
    }

    if (pipe(ready_pipe) != 0 || pipe(release_pipe) != 0) {
        note_fail("pipe", strerror(errno));
        if (ready_pipe[0] >= 0) {
            close(ready_pipe[0]);
        }
        if (ready_pipe[1] >= 0) {
            close(ready_pipe[1]);
        }
        if (release_pipe[0] >= 0) {
            close(release_pipe[0]);
        }
        if (release_pipe[1] >= 0) {
            close(release_pipe[1]);
        }
        remove_dir_if_exists(dir);
        return;
    }

    pid_t child = fork();
    if (child < 0) {
        note_fail("fork", strerror(errno));
        close(ready_pipe[0]);
        close(ready_pipe[1]);
        close(release_pipe[0]);
        close(release_pipe[1]);
        remove_dir_if_exists(dir);
        return;
    }

    if (child == 0) {
        char token;

        close(ready_pipe[0]);
        close(release_pipe[1]);
        if (chdir(dir) != 0) {
            _exit(10);
        }
        if (write(ready_pipe[1], "R", 1) != 1) {
            _exit(11);
        }
        close(ready_pipe[1]);
        if (read(release_pipe[0], &token, 1) != 1) {
            _exit(12);
        }
        close(release_pipe[0]);
        _exit(0);
    }

    close(ready_pipe[1]);
    close(release_pipe[0]);

    char token;
    int child_ready = read(ready_pipe[0], &token, 1) == 1;
    if (child_ready) {
        note_pass("child entered tmpfs cwd");
    } else {
        note_fail("wait child cwd ready", strerror(errno));
    }
    close(ready_pipe[0]);

    if (child_ready) {
        if (rmdir(dir) == 0) {
            note_pass("rmdir tmpfs dir while child uses cwd");
        } else {
            note_fail("rmdir tmpfs active cwd dir", strerror(errno));
        }

        if (write(release_pipe[1], "X", 1) != 1) {
            note_fail("release child", strerror(errno));
        }
    }
    close(release_pipe[1]);

    int status = 0;
    pid_t waited = waitpid(child, &status, 0);
    if (waited == child && WIFEXITED(status) && WEXITSTATUS(status) == 0) {
        note_pass("child can exit after tmpfs cwd unlink");
    } else {
        char detail[160];
        snprintf(detail, sizeof(detail), "waited=%d errno=%d status=0x%x",
                 waited, errno, status);
        note_fail("child tmpfs unlinked cwd exit", detail);
    }

    remove_dir_if_exists(dir);
}

int main(void)
{
    printf("=== bug-tmpfs-cwd-drop-safe ===\n");

    for (int i = 0; i < 8; i++) {
        expect_child_cwd_drop_is_safe();
    }

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("ALL TESTS PASSED\n");
        return 0;
    }
    printf("SOME TESTS FAILED\n");
    return 1;
}
