#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef CLOSE_RANGE_CLOEXEC
#define CLOSE_RANGE_CLOEXEC (1U << 2)
#endif

#define FCNTL_LOCK_PATH "/tmp/lock-close-range-fcntl.lock"
#define FLOCK_LOCK_PATH "/tmp/lock-close-range-flock.lock"

static void fail(const char *msg)
{
    printf("LOCK_CLOSE_RANGE_CLOEXEC_FAILED: %s errno=%d (%s)\n", msg, errno, strerror(errno));
    exit(1);
}

static void checked_write(int fd)
{
    char ch = 'R';
    if (write(fd, &ch, 1) != 1) {
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

static void set_fcntl_lock(int fd, int cmd)
{
    struct flock fl = {
        .l_type = F_WRLCK,
        .l_whence = SEEK_SET,
        .l_start = 0,
        .l_len = 0,
    };
    if (fcntl(fd, cmd, &fl) != 0) {
        fail("fcntl lock");
    }
}

static int try_fcntl_lock(int fd)
{
    struct flock fl = {
        .l_type = F_WRLCK,
        .l_whence = SEEK_SET,
        .l_start = 0,
        .l_len = 0,
    };
    return fcntl(fd, F_SETLK, &fl);
}

static void child_exec_after_close_range(const char *path, int pipe_write, int use_flock)
{
    int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0600);
    if (fd < 0) {
        fail("child open lock file");
    }

    if (use_flock) {
        if (flock(fd, LOCK_EX) != 0) {
            fail("child flock lock");
        }
    } else {
        set_fcntl_lock(fd, F_SETLK);
    }

    if (syscall(SYS_close_range, (unsigned int)fd, (unsigned int)fd, CLOSE_RANGE_CLOEXEC) != 0) {
        fail("child close_range CLOEXEC");
    }

    checked_write(pipe_write);
    close(pipe_write);
    execl("/bin/sleep", "sleep", "1", NULL);
    fail("exec /bin/sleep");
}

static void run_case(const char *path, int use_flock)
{
    unlink(path);

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
        child_exec_after_close_range(path, pipefd[1], use_flock);
    }

    close(pipefd[1]);
    checked_read(pipefd[0]);
    close(pipefd[0]);

    usleep(200000);

    int fd = open(path, O_RDWR);
    if (fd < 0) {
        fail("parent open lock file");
    }
    if (use_flock) {
        if (flock(fd, LOCK_EX | LOCK_NB) != 0) {
            fail("parent flock after close_range CLOEXEC exec");
        }
        printf("LOCK_CLOSE_RANGE_FLOCK_PASSED\n");
    } else {
        if (try_fcntl_lock(fd) != 0) {
            fail("parent fcntl after close_range CLOEXEC exec");
        }
        printf("LOCK_CLOSE_RANGE_FCNTL_PASSED\n");
    }

    int status = 0;
    if (waitpid(child, &status, 0) < 0) {
        fail("waitpid");
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        printf("LOCK_CLOSE_RANGE_CLOEXEC_FAILED: child status=%d\n", status);
        exit(1);
    }

    close(fd);
    unlink(path);
}

int main(void)
{
    run_case(FCNTL_LOCK_PATH, 0);
    run_case(FLOCK_LOCK_PATH, 1);
    printf("LOCK_CLOSE_RANGE_CLOEXEC_ALL_PASSED\n");
    return 0;
}
