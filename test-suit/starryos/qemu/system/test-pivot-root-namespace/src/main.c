#define _GNU_SOURCE
#include <errno.h>
#include <sched.h>
#include <stdio.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef SYS_pivot_root
#define SYS_pivot_root __NR_pivot_root
#endif

static long pivot_root_call(const char *new_root, const char *put_old) {
    return syscall(SYS_pivot_root, new_root, put_old);
}

static int child_body(int ready_fd) {
    if (unshare(CLONE_NEWNS) < 0) {
        perror("child: unshare(CLONE_NEWNS)");
        return 1;
    }
    if (mkdir("/tmp/pivot-ns", 0755) < 0 && errno != EEXIST) {
        perror("child: mkdir /tmp/pivot-ns");
        return 1;
    }
    if (mkdir("/tmp/pivot-ns/old", 0700) < 0 && errno != EEXIST) {
        perror("child: mkdir /tmp/pivot-ns/old");
        return 1;
    }
    if (mount("/tmp/pivot-ns", "/tmp/pivot-ns", NULL, MS_BIND, NULL) < 0) {
        perror("child: bind /tmp/pivot-ns");
        return 1;
    }
    if (chdir("/tmp/pivot-ns") < 0) {
        perror("child: chdir /tmp/pivot-ns");
        return 1;
    }
    if (pivot_root_call(".", "old") < 0) {
        perror("child: pivot_root");
        return 1;
    }
    if (write(ready_fd, "1", 1) != 1) {
        perror("child: write ready");
        return 1;
    }
    return 0;
}

int main(void) {
    int ready[2];
    if (pipe(ready) < 0) {
        perror("pipe");
        return 1;
    }

    pid_t pid = fork();
    if (pid < 0) {
        perror("fork");
        return 1;
    }
    if (pid == 0) {
        close(ready[0]);
        _exit(child_body(ready[1]));
    }

    close(ready[1]);
    char marker;
    if (read(ready[0], &marker, 1) != 1) {
        fprintf(stderr, "FAIL: child did not complete private namespace pivot\n");
        return 1;
    }
    if (access("/bin/sh", X_OK) < 0) {
        fprintf(stderr, "FAIL: parent root/cwd changed by child namespace pivot errno=%d\n", errno);
        return 1;
    }

    int status;
    if (waitpid(pid, &status, 0) < 0 || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fprintf(stderr, "FAIL: child status=%d\n", status);
        return 1;
    }

    printf("TEST_PIVOT_ROOT_NAMESPACE_PASSED\n");
    return 0;
}
