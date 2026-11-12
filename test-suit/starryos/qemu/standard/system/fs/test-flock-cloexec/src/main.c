#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/wait.h>
#include <unistd.h>

#define LOCK_PATH "/tmp/flock-cloexec.lock"

static void fail(const char *msg)
{
    printf("FLOCK_CLOEXEC_FAILED: %s errno=%d (%s)\n", msg, errno, strerror(errno));
    exit(1);
}

static void checked_write(int fd, const char *msg)
{
    if (write(fd, msg, 1) != 1) {
        fail("pipe write");
    }
}

static void checked_read(int fd)
{
    char ch;
    if (read(fd, &ch, 1) != 1) {
        fail("pipe read");
    }
}

static void child_lock_cloexec_exec(int pipe_write)
{
    int fd = open(LOCK_PATH, O_CREAT | O_RDWR | O_TRUNC, 0600);
    if (fd < 0) {
        fail("child open lock file");
    }
    if (flock(fd, LOCK_EX) != 0) {
        fail("child flock lock");
    }
    int flags = fcntl(fd, F_GETFD);
    if (flags < 0) {
        fail("child F_GETFD");
    }
    if (fcntl(fd, F_SETFD, flags | FD_CLOEXEC) != 0) {
        fail("child F_SETFD FD_CLOEXEC");
    }

    checked_write(pipe_write, "R");
    close(pipe_write);
    execl("/bin/sleep", "sleep", "1", NULL);
    fail("exec /bin/sleep");
}

int main(void)
{
    unlink(LOCK_PATH);

    int pipefd[2];
    if (pipe(pipefd) != 0) {
        fail("pipe");
    }

    pid_t child = fork();
    if (child < 0) {
        fail("fork");
    }
    if (child == 0) {
        close(pipefd[0]);
        child_lock_cloexec_exec(pipefd[1]);
    }

    close(pipefd[1]);
    checked_read(pipefd[0]);
    close(pipefd[0]);

    usleep(200000);

    int fd = open(LOCK_PATH, O_RDWR);
    if (fd < 0) {
        fail("parent open lock file");
    }
    if (flock(fd, LOCK_EX | LOCK_NB) != 0) {
        fail("parent flock after child exec CLOEXEC");
    }
    printf("FLOCK_CLOEXEC_PARENT_LOCK_PASSED\n");

    int status = 0;
    if (waitpid(child, &status, 0) < 0) {
        fail("waitpid");
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        printf("FLOCK_CLOEXEC_FAILED: child status=%d\n", status);
        return 1;
    }

    close(fd);
    unlink(LOCK_PATH);
    printf("FLOCK_CLOEXEC_ALL_PASSED\n");
    return 0;
}
