#define _GNU_SOURCE

#include <errno.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static void fail(const char *stage)
{
    printf("TEST_NIX_BUILDER_EXEC_FAILED stage=%s errno=%d (%s)\n", stage,
           errno, strerror(errno));
    exit(1);
}

static void timeout_handler(int signo)
{
    (void)signo;
    static const char marker[] =
        "TEST_NIX_BUILDER_EXEC_FAILED stage=timeout\n";
    ssize_t ignored = write(STDOUT_FILENO, marker, sizeof(marker) - 1);
    (void)ignored;
    _exit(1);
}

static void write_all(int fd, const void *buffer, size_t length)
{
    const char *cursor = buffer;
    while (length > 0) {
        ssize_t written = write(fd, cursor, length);
        if (written < 0) {
            if (errno == EINTR)
                continue;
            fail("write-child-pid");
        }
        cursor += written;
        length -= (size_t)written;
    }
}

static void read_all(int fd, void *buffer, size_t length)
{
    char *cursor = buffer;
    while (length > 0) {
        ssize_t count = read(fd, cursor, length);
        if (count < 0) {
            if (errno == EINTR)
                continue;
            fail("read-child-pid");
        }
        if (count == 0) {
            errno = EPIPE;
            fail("read-child-pid-eof");
        }
        cursor += count;
        length -= (size_t)count;
    }
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    signal(SIGALRM, timeout_handler);
    alarm(10);

    int pid_pipe[2];
    if (pipe(pid_pipe) < 0)
        fail("pipe");

    pid_t helper = fork();
    if (helper < 0)
        fail("fork-helper");
    if (helper == 0) {
        close(pid_pipe[0]);
        pid_t builder = (pid_t)syscall(__NR_clone, CLONE_PARENT | SIGCHLD,
                                      NULL, NULL, NULL, 0UL);
        if (builder < 0)
            fail("clone-parent");
        if (builder == 0) {
            char *const argv[] = { "/bin/sh", "-c", "exit 37", NULL };
            execv(argv[0], argv);
            _exit(127);
        }
        write_all(pid_pipe[1], &builder, sizeof(builder));
        _exit(0);
    }

    close(pid_pipe[1]);
    pid_t builder;
    read_all(pid_pipe[0], &builder, sizeof(builder));
    close(pid_pipe[0]);

    int status;
    if (waitpid(helper, &status, 0) != helper || !WIFEXITED(status) ||
        WEXITSTATUS(status) != 0) {
        errno = ECHILD;
        fail("wait-helper");
    }
    if (waitpid(builder, &status, 0) != builder || !WIFEXITED(status) ||
        WEXITSTATUS(status) != 37) {
        errno = ECHILD;
        fail("wait-builder-exec");
    }

    alarm(0);
    puts("TEST_NIX_BUILDER_EXEC_PASSED");
    return 0;
}
