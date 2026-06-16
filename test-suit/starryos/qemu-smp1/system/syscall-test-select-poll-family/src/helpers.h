#pragma once

#include <unistd.h>
#include <fcntl.h>
#include <sys/types.h>
#include <sys/stat.h>
#include <sys/select.h>
#include <poll.h>

int create_pipe(int fds[2]);
int create_nonblocking_pipe(int fds[2]);
int write_exact(int fd, const void *buf, size_t count);
int read_exact(int fd, void *buf, size_t count);
int fill_pipe(int write_fd);
long long time_ms(void);
int create_temp_file(const char *path);

long raw_select(int nfds, fd_set *rfds, fd_set *wfds, fd_set *efds, struct timeval *tv);
long raw_poll(struct pollfd *fds, nfds_t nfds, int timeout);
