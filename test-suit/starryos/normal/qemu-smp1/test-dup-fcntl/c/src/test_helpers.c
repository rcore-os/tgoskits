#include "test_helpers.h"
#include <stdio.h>
#include <string.h>
#include <errno.h>
#include <unistd.h>
#include <fcntl.h>
#include <sys/file.h>

void safe_close(int *fd)
{
    if (fd && *fd >= 0) {
        close(*fd);
        *fd = -1;
    }
}

int errno_is_lock_conflict(int err)
{
    return err == EAGAIN || err == EACCES;
}

int errno_is_wouldblock(int err)
{
    return err == EWOULDBLOCK || err == EAGAIN;
}

int dupfd_at_least(int fd, int minfd)
{
    return fcntl(fd, F_DUPFD, minfd);
}

int create_temp_file_with_data(const char *path, const char *data)
{
    int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0644);
    if (fd < 0) return -1;
    if (data) {
        ssize_t n = write(fd, data, strlen(data));
        if (n < 0) {
            close(fd);
            return -1;
        }
    }
    close(fd);
    return 0;
}

int read_all_and_compare(int fd, const char *expect, size_t n)
{
    char buf[256];
    if (n > sizeof(buf)) return -1;
    ssize_t r = read(fd, buf, n);
    if (r != (ssize_t)n) return -1;
    return memcmp(buf, expect, n) == 0 ? 0 : -1;
}

int sync_pipe_create(int fds[2])
{
    return pipe(fds);
}

int sync_pipe_signal(int *write_fd)
{
    char c = 1;
    return (int)write(*write_fd, &c, 1);
}

int sync_pipe_wait(int *read_fd)
{
    char c;
    ssize_t r = read(*read_fd, &c, 1);
    return (r == 1) ? 0 : -1;
}
