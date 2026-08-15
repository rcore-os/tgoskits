#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/file.h>
#include <sys/prctl.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#define LOCK_PATH "/tmp/starry-case-task-isolation.lock"
#define LEAK_COMM "case-leak-child"

static int acquire_lifetime_lock(void)
{
    int fd = open(LOCK_PATH, O_RDWR | O_CREAT, 0600);
    if (fd < 0) {
        return -1;
    }
    if (flock(fd, LOCK_EX) != 0) {
        int saved_errno = errno;
        close(fd);
        errno = saved_errno;
        return -1;
    }
    return fd;
}

static void stop_child(pid_t child)
{
    (void)kill(child, SIGKILL);
    while (waitpid(child, NULL, 0) < 0 && errno == EINTR) {
    }
}

int main(void)
{
    int ready[2];
    if (pipe(ready) != 0) {
        perror("pipe");
        return 1;
    }
    int blocker[2];
    if (pipe(blocker) != 0) {
        perror("blocker pipe");
        close(ready[0]);
        close(ready[1]);
        return 1;
    }
    (void)unlink(LOCK_PATH);

    pid_t child = fork();
    if (child < 0) {
        perror("fork");
        close(ready[0]);
        close(ready[1]);
        close(blocker[0]);
        close(blocker[1]);
        return 1;
    }
    if (child == 0) {
        close(ready[0]);
        if (setsid() < 0 || prctl(PR_SET_NAME, LEAK_COMM, 0, 0, 0) != 0) {
            _exit(2);
        }
        int lock_fd = acquire_lifetime_lock();
        if (lock_fd < 0) {
            _exit(2);
        }
        char token = 'R';
        if (write(ready[1], &token, sizeof(token)) != (ssize_t)sizeof(token)) {
            _exit(3);
        }
        close(ready[1]);
        (void)lock_fd;
        /* Keep our own write end open so the read cannot complete with EOF.
         * PID namespace shutdown must force-wake this raw blocking wait after
         * publishing SIGKILL; task.interrupt() alone is insufficient. */
        char never;
        (void)read(blocker[0], &never, sizeof(never));
        _exit(4);
    }

    close(ready[1]);
    close(blocker[0]);
    close(blocker[1]);
    char token = 0;
    ssize_t received;
    do {
        received = read(ready[0], &token, sizeof(token));
    } while (received < 0 && errno == EINTR);
    close(ready[0]);
    if (received != (ssize_t)sizeof(token) || token != 'R') {
        fprintf(stderr, "case-isolation child did not publish its marker\n");
        stop_child(child);
        return 1;
    }

    printf("CASE_TASK_ISOLATION_LEAK_PUBLISHED\n");
    return 0;
}
