#define _GNU_SOURCE
#include <errno.h>
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#define BUF_SIZE 65536

static char *read_file(const char *path, char *buf, size_t size) {
    FILE *f = fopen(path, "r");
    if (!f) return NULL;
    size_t n = fread(buf, 1, size - 1, f);
    fclose(f);
    buf[n] = '\0';
    return buf;
}

int main(void) {
    mkdir("/mnt", 0755);

    int ready_pipe[2];
    int release_pipe[2];
    if (pipe(ready_pipe) < 0 || pipe(release_pipe) < 0) {
        perror("pipe");
        return 1;
    }

    pid_t pid = fork();
    if (pid < 0) {
        perror("fork");
        return 1;
    }

    if (pid == 0) {
        close(ready_pipe[0]);
        close(release_pipe[1]);

        /* Child: unshare mount namespace */
        if (unshare(CLONE_NEWNS) < 0) {
            perror("unshare(CLONE_NEWNS)");
            _exit(1);
        }

        /* Mount tmpfs at /mnt */
        if (mount("tmpfs", "/mnt", "tmpfs", 0, NULL) < 0) {
            perror("mount tmpfs /mnt");
            _exit(1);
        }

        /* Verify /mnt appears in child's /proc/self/mounts */
        char buf[BUF_SIZE];
        if (!read_file("/proc/self/mounts", buf, sizeof(buf))) {
            fprintf(stderr, "child: cannot read /proc/self/mounts\n");
            _exit(1);
        }

        if (strstr(buf, "/mnt") == NULL) {
            fprintf(stderr, "child: /mnt not in mounts after mount\n");
            _exit(1);
        }

        char ready = 'R';
        if (write(ready_pipe[1], &ready, 1) != 1) {
            perror("write mount-ready");
            _exit(1);
        }

        char release;
        if (read(release_pipe[0], &release, 1) != 1) {
            perror("read mount-release");
            _exit(1);
        }

        /* Tear the private mount down explicitly before process exit. */
        if (umount("/mnt") < 0) {
            perror("umount /mnt");
            _exit(1);
        }

        _exit(0);
    }

    close(ready_pipe[1]);
    close(release_pipe[0]);

    char ready = 0;
    int child_ready = read(ready_pipe[0], &ready, 1) == 1 && ready == 'R';
    close(ready_pipe[0]);
    int parent_ok = child_ready;
    if (!child_ready) {
        fputs("FAIL: child did not finish its private mount setup\n", stderr);
    }

    /* Check isolation while the child mount is still alive. */
    if (parent_ok) {
        char buf[BUF_SIZE];
        if (!read_file("/proc/self/mounts", buf, sizeof(buf))) {
            fprintf(stderr, "parent: cannot read /proc/self/mounts\n");
            parent_ok = 0;
        } else if (strstr(buf, "/mnt") != NULL) {
            fprintf(stderr, "FAIL: /mnt leaked into parent namespace\n");
            parent_ok = 0;
        }
    }

    /*
     * Once the ready byte has arrived, the child is blocked on release_pipe.
     * Always release it, even when the parent-side assertion failed, so a
     * diagnostic failure cannot turn into an unbounded waitpid hang.
     */
    if (child_ready && write(release_pipe[1], "R", 1) != 1) {
        perror("write mount-release");
        parent_ok = 0;
    }
    close(release_pipe[1]);

    int status;
    waitpid(pid, &status, 0);
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fprintf(stderr, "FAIL: child exited with status %d\n", status);
        parent_ok = 0;
    }
    if (!parent_ok)
        return 1;

    rmdir("/mnt");
    printf("TEST_PER_NS_MOUNTS_PASSED\n");
    return 0;
}
