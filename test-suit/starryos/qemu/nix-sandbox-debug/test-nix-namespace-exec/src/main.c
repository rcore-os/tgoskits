#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

#define CHILD_STACK_SIZE (64 * 1024)

struct child_context {
    int sync_read;
    const char *executable;
};

static void fail(const char *stage)
{
    printf("TEST_NIX_NAMESPACE_EXEC_FAILED stage=%s errno=%d (%s)\n", stage,
           errno, strerror(errno));
    exit(1);
}

static void child_fail(const char *stage)
{
    dprintf(STDOUT_FILENO,
            "TEST_NIX_NAMESPACE_EXEC_FAILED stage=%s errno=%d (%s)\n",
            stage, errno, strerror(errno));
    _exit(1);
}

static void timeout_handler(int signo)
{
    (void)signo;
    static const char marker[] =
        "TEST_NIX_NAMESPACE_EXEC_FAILED stage=timeout\n";
    ssize_t ignored = write(STDOUT_FILENO, marker, sizeof(marker) - 1);
    (void)ignored;
    _exit(1);
}

static void write_all(int fd, const void *buffer, size_t length,
                      const char *stage)
{
    const char *cursor = buffer;
    while (length > 0) {
        ssize_t written = write(fd, cursor, length);
        if (written < 0) {
            if (errno == EINTR)
                continue;
            fail(stage);
        }
        cursor += written;
        length -= (size_t)written;
    }
}

static void read_all(int fd, void *buffer, size_t length, const char *stage)
{
    char *cursor = buffer;
    while (length > 0) {
        ssize_t count = read(fd, cursor, length);
        if (count < 0) {
            if (errno == EINTR)
                continue;
            fail(stage);
        }
        if (count == 0) {
            errno = EPIPE;
            fail(stage);
        }
        cursor += count;
        length -= (size_t)count;
    }
}

static void write_proc_file(pid_t pid, const char *name, const char *value)
{
    char path[128];
    int length = snprintf(path, sizeof(path), "/proc/%d/%s", pid, name);
    if (length < 0 || (size_t)length >= sizeof(path)) {
        errno = ENAMETOOLONG;
        fail(name);
    }

    int fd = open(path, O_WRONLY);
    if (fd < 0)
        fail(name);
    write_all(fd, value, strlen(value), name);
    if (close(fd) < 0)
        fail(name);
}

static int namespace_child(void *opaque)
{
    const struct child_context *context = opaque;
    char token;
    ssize_t count;
    do {
        count = read(context->sync_read, &token, 1);
    } while (count < 0 && errno == EINTR);
    if (count != 1 || token != '1')
        child_fail("sync-read");
    close(context->sync_read);

    char *const arguments[] = { (char *)context->executable,
                                "--namespace-child", NULL };
    execv(arguments[0], arguments);
    child_fail("execv");
    return 1;
}

static int namespace_child_after_exec(void)
{
    puts("TEST_NIX_NAMESPACE_EXEC_CHILD_AFTER_EXEC");
    return 37;
}

int main(int argc, char **argv)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    if (argc == 2 && strcmp(argv[1], "--namespace-child") == 0)
        return namespace_child_after_exec();

    signal(SIGALRM, timeout_handler);
    alarm(10);

    int sync_pipe[2];
    int pid_pipe[2];
    if (pipe(sync_pipe) < 0 || pipe(pid_pipe) < 0)
        fail("pipes");

    pid_t helper = fork();
    if (helper < 0)
        fail("fork-helper");
    if (helper == 0) {
        close(sync_pipe[1]);
        close(pid_pipe[0]);

        void *stack = mmap(NULL, CHILD_STACK_SIZE, PROT_READ | PROT_WRITE,
                           MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (stack == MAP_FAILED)
            child_fail("mmap-stack");
        struct child_context context = {
            .sync_read = sync_pipe[0],
            .executable = argv[0],
        };
        int flags = CLONE_PARENT | CLONE_NEWPID | CLONE_NEWUSER | CLONE_NEWNS |
                    CLONE_NEWIPC | CLONE_NEWUTS | CLONE_NEWNET | SIGCHLD;
        pid_t child = clone(namespace_child,
                            (char *)stack + CHILD_STACK_SIZE, flags, &context);
        if (child < 0)
            child_fail("clone-namespaces");
        write_all(pid_pipe[1], &child, sizeof(child), "send-child-pid");
        _exit(0);
    }

    close(sync_pipe[0]);
    close(pid_pipe[1]);

    pid_t child;
    read_all(pid_pipe[0], &child, sizeof(child), "receive-child-pid");
    close(pid_pipe[0]);

    char mapping[64];
    int length = snprintf(mapping, sizeof(mapping), "0 %u 1\n", getuid());
    if (length < 0 || (size_t)length >= sizeof(mapping)) {
        errno = EOVERFLOW;
        fail("uid-map-format");
    }
    write_proc_file(child, "uid_map", mapping);
    write_proc_file(child, "setgroups", "deny\n");
    length = snprintf(mapping, sizeof(mapping), "0 %u 1\n", getgid());
    if (length < 0 || (size_t)length >= sizeof(mapping)) {
        errno = EOVERFLOW;
        fail("gid-map-format");
    }
    write_proc_file(child, "gid_map", mapping);

    write_all(sync_pipe[1], "1", 1, "release-child");
    close(sync_pipe[1]);

    int status;
    if (waitpid(helper, &status, 0) != helper || !WIFEXITED(status) ||
        WEXITSTATUS(status) != 0) {
        errno = ECHILD;
        fail("wait-helper");
    }
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status) ||
        WEXITSTATUS(status) != 37) {
        errno = ECHILD;
        fail("wait-namespace-child");
    }

    alarm(0);
    puts("TEST_NIX_NAMESPACE_EXEC_PASSED");
    return 0;
}
