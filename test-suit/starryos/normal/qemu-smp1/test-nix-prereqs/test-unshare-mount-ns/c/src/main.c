#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef __NR_setns
#error "__NR_setns required from <sys/syscall.h>"
#endif

#define BASE "/tmp/nix-prereq-mnt-ns"
#define SOURCE BASE "/source"
#define TARGET BASE "/target"
#define SOURCE_MARKER SOURCE "/setns-visible"
#define TARGET_MARKER TARGET "/setns-visible"

#define FAIL(msg)                                                              \
    do {                                                                       \
        fprintf(stderr, "FAIL | %s:%d | %s: %s\n", __FILE__, __LINE__, msg,    \
                strerror(errno));                                              \
        exit(1);                                                               \
    } while (0)

#define PASS(msg)                                                              \
    do {                                                                       \
        printf("  PASS | %s:%d | %s\n", __FILE__, __LINE__, msg);             \
    } while (0)

static int xsetns(int fd, int nstype) {
    return (int)syscall(__NR_setns, fd, nstype);
}

static void write_all(int fd, const char *buf, size_t len, const char *what) {
    size_t done = 0;
    while (done < len) {
        ssize_t n = write(fd, buf + done, len - done);
        if (n < 0)
            FAIL(what);
        done += (size_t)n;
    }
}

static void read_one(int fd, const char *what) {
    char byte;
    ssize_t n = read(fd, &byte, 1);
    if (n != 1)
        FAIL(what);
}

static void prepare_tree(void) {
    mkdir(BASE, 0755);
    mkdir(SOURCE, 0755);
    mkdir(TARGET, 0755);

    int fd = open(SOURCE_MARKER, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    if (fd < 0)
        FAIL("create source marker");
    write_all(fd, "mount namespace marker\n", 23, "write source marker");
    if (close(fd) < 0)
        FAIL("close source marker");

    if (access(SOURCE, F_OK) < 0)
        FAIL("source directory exists");
    if (access(TARGET, F_OK) < 0)
        FAIL("target directory exists");
    if (access(TARGET_MARKER, F_OK) == 0) {
        errno = EEXIST;
        FAIL("parent target starts without marker");
    }
    if (errno != ENOENT)
        FAIL("check parent target marker absence");
}

static void child_body(int ready_fd, int release_fd) {
    if (unshare(CLONE_NEWNS) < 0)
        FAIL("unshare(CLONE_NEWNS)");
    PASS("child unshared mount namespace");

    if (mount(SOURCE, TARGET, "none", MS_BIND, NULL) < 0)
        FAIL("bind mount source onto target");
    if (access(TARGET_MARKER, F_OK) < 0)
        FAIL("child sees marker through namespace-local mount");
    PASS("child sees namespace-local bind mount");

    write_all(ready_fd, "R", 1, "signal child mount ready");
    read_one(release_fd, "wait parent setns check");

    if (umount2(TARGET, MNT_DETACH) < 0)
        FAIL("detach child bind mount");
    exit(0);
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("================================================\n");
    printf("  TEST: unshare/setns(CLONE_NEWNS) mount view\n");
    printf("================================================\n");

    prepare_tree();

    int ready_pipe[2];
    int release_pipe[2];
    if (pipe(ready_pipe) < 0)
        FAIL("pipe ready");
    if (pipe(release_pipe) < 0)
        FAIL("pipe release");

    pid_t child = fork();
    if (child < 0)
        FAIL("fork");

    if (child == 0) {
        close(ready_pipe[0]);
        close(release_pipe[1]);
        child_body(ready_pipe[1], release_pipe[0]);
    }

    close(ready_pipe[1]);
    close(release_pipe[0]);
    read_one(ready_pipe[0], "wait child mount ready");

    if (access(TARGET_MARKER, F_OK) == 0) {
        errno = EEXIST;
        FAIL("parent original namespace must not see child bind mount");
    }
    if (errno != ENOENT)
        FAIL("check original namespace target marker absence");
    PASS("parent original namespace does not see child bind mount");

    char ns_path[64];
    snprintf(ns_path, sizeof(ns_path), "/proc/%d/ns/mnt", child);
    int nsfd = open(ns_path, O_RDONLY | O_CLOEXEC);
    if (nsfd < 0)
        FAIL("open child /proc/<pid>/ns/mnt");
    if (xsetns(nsfd, CLONE_NEWNS) < 0)
        FAIL("setns child mount namespace");
    if (close(nsfd) < 0)
        FAIL("close nsfd");
    if (access(TARGET_MARKER, F_OK) < 0)
        FAIL("parent sees child bind mount after setns");
    PASS("setns(CLONE_NEWNS) switches to target mount view");

    write_all(release_pipe[1], "D", 1, "release child");

    int status;
    if (waitpid(child, &status, 0) < 0)
        FAIL("waitpid child");
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0)
        FAIL("child exited non-zero");
    PASS("child exited cleanly");

    printf("UNSHARE_MOUNT_NS_ALL_PASSED\n");
    return 0;
}
