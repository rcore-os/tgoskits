#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/file.h>
#include <unistd.h>

#define LOCK_PATH "/tmp/starry-case-task-isolation.lock"

int main(void)
{
    int fd = open(LOCK_PATH, O_RDWR);
    if (fd < 0) {
        perror("open case-isolation lock");
        return 1;
    }
    if (flock(fd, LOCK_EX | LOCK_NB) != 0) {
        int saved_errno = errno;
        close(fd);
        (void)unlink(LOCK_PATH);
        if (saved_errno == EWOULDBLOCK) {
            fprintf(stderr,
                    "CASE_TASK_ISOLATION_FAILED: descendant from previous case still owns lock\n");
        } else {
            errno = saved_errno;
            perror("acquire case-isolation lock");
        }
        return 1;
    }
    if (flock(fd, LOCK_UN) != 0) {
        perror("release case-isolation lock");
        close(fd);
        (void)unlink(LOCK_PATH);
        return 1;
    }
    close(fd);
    (void)unlink(LOCK_PATH);
    printf("CASE_TASK_ISOLATION_PASSED\n");
    return 0;
}
