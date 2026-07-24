#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#define CLONE_NEWCGROUP 0x02000000
#define STACK_SIZE (1024 * 1024)

static unsigned long long get_ns_id(const char *path)
{
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        fprintf(stderr, "FAIL: open(%s): %s\n", path, strerror(errno));
        return 0;
    }
    struct stat st;
    if (fstat(fd, &st) != 0) {
        fprintf(stderr, "FAIL: fstat(%s): %s\n", path, strerror(errno));
        close(fd);
        return 0;
    }
    close(fd);
    return (unsigned long long)st.st_ino;
}

static int child_fn(void *arg)
{
    unsigned long long parent_id = *(const unsigned long long *)arg;
    unsigned long long id_child = get_ns_id("/proc/self/ns/cgroup");
    if (id_child == 0) {
        _exit(1);
    }
    if (id_child == parent_id) {
        fprintf(stderr,
                "FAIL: child cgroup ns id matches parent (%llu)\n",
                id_child);
        _exit(1);
    }
    printf("INFO: child cgroup ns id: %llu\n", id_child);
    _exit(0);
}

int main(void)
{
    unsigned long long id_before = get_ns_id("/proc/self/ns/cgroup");
    if (id_before == 0) {
        return 1;
    }
    printf("INFO: initial cgroup ns id: %llu\n", id_before);

    int rc = unshare(CLONE_NEWCGROUP);
    if (rc != 0) {
        fprintf(stderr, "FAIL: unshare(CLONE_NEWCGROUP): %s\n", strerror(errno));
        return 1;
    }

    unsigned long long id_after_unshare = get_ns_id("/proc/self/ns/cgroup");
    if (id_after_unshare == 0) {
        return 1;
    }
    printf("INFO: after unshare cgroup ns id: %llu\n", id_after_unshare);

    if (id_after_unshare == id_before) {
        fprintf(stderr,
                "FAIL: cgroup ns id did not change after unshare (%llu)\n",
                id_after_unshare);
        return 1;
    }

    char *stack = malloc(STACK_SIZE);
    if (!stack) {
        perror("malloc");
        return 1;
    }
    char *stack_top = stack + STACK_SIZE;

    pid_t pid = clone(child_fn, stack_top, CLONE_NEWCGROUP | SIGCHLD,
                      &id_after_unshare);
    if (pid < 0) {
        fprintf(stderr, "FAIL: clone(CLONE_NEWCGROUP): %s\n", strerror(errno));
        free(stack);
        return 1;
    }

    int status = 0;
    if (waitpid(pid, &status, 0) < 0) {
        perror("waitpid");
        free(stack);
        return 1;
    }
    free(stack);
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fprintf(stderr, "FAIL: child exited with status %d\n", status);
        return 1;
    }

    int nsfd = open("/proc/self/ns/cgroup", O_RDONLY);
    if (nsfd < 0) {
        perror("open /proc/self/ns/cgroup");
        return 1;
    }

    rc = unshare(CLONE_NEWCGROUP);
    if (rc != 0) {
        fprintf(stderr, "FAIL: second unshare: %s\n", strerror(errno));
        close(nsfd);
        return 1;
    }

    unsigned long long id_after_second_unshare = get_ns_id("/proc/self/ns/cgroup");
    if (id_after_second_unshare == id_after_unshare) {
        fprintf(stderr, "FAIL: second unshare did not change ns id\n");
        close(nsfd);
        return 1;
    }

    rc = setns(nsfd, CLONE_NEWCGROUP);
    if (rc != 0) {
        fprintf(stderr, "FAIL: setns(CLONE_NEWCGROUP): %s\n", strerror(errno));
        close(nsfd);
        return 1;
    }
    close(nsfd);

    unsigned long long id_after_setns = get_ns_id("/proc/self/ns/cgroup");
    if (id_after_setns != id_after_unshare) {
        fprintf(stderr,
                "FAIL: setns did not restore ns id (%llu != %llu)\n",
                id_after_setns, id_after_unshare);
        return 1;
    }

    printf("TEST_CGROUP_NS_PASSED\n");
    return 0;
}
