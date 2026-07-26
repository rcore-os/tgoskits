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

    pid_t pid = fork();
    if (pid < 0) {
        perror("fork");
        return 1;
    }

    if (pid == 0) {
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

        _exit(0);
    }

    /* Parent: wait for child */
    int status;
    waitpid(pid, &status, 0);
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fprintf(stderr, "FAIL: child exited with status %d\n", status);
        return 1;
    }

    /* Verify /mnt does NOT appear in parent's /proc/self/mounts */
    char buf[BUF_SIZE];
    if (!read_file("/proc/self/mounts", buf, sizeof(buf))) {
        fprintf(stderr, "parent: cannot read /proc/self/mounts\n");
        return 1;
    }

    if (strstr(buf, "/mnt") != NULL) {
        fprintf(stderr, "FAIL: /mnt leaked into parent namespace\n");
        return 1;
    }

    rmdir("/mnt");
    printf("TEST_PER_NS_MOUNTS_PASSED\n");
    return 0;
}
