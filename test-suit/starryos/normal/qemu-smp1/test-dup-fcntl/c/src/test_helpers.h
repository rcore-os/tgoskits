#pragma once

#include <unistd.h>
#include <fcntl.h>
#include <sys/file.h>

#define TMPFILE "/tmp/starry_test_dup_v2"

/* 关闭 fd 并置 -1，避免 double-close */
void safe_close(int *fd);

/* 判断 errno 是否为锁冲突（EAGAIN || EACCES） */
int errno_is_lock_conflict(int err);

/* 判断 errno 是否为 wouldblock（EWOULDBLOCK || EAGAIN） */
int errno_is_wouldblock(int err);

/* 用 fcntl(F_DUPFD, minfd) 生成 >= minfd 的 fd */
int dupfd_at_least(int fd, int minfd);

/* 创建临时文件并写入数据，返回 0 成功 / -1 失败 */
int create_temp_file_with_data(const char *path, const char *data);

/* 从 fd 读取 n 字节并与 expect 比较，返回 0 一致 / -1 不一致 */
int read_all_and_compare(int fd, const char *expect, size_t n);

/* 创建同步 pipe，fds[0]=读端, fds[1]=写端 */
int sync_pipe_create(int fds[2]);

/* 向 pipe 写 1 字节通知对方 */
int sync_pipe_signal(int *write_fd);

/* 阻塞等待 pipe 可读 */
int sync_pipe_wait(int *read_fd);
