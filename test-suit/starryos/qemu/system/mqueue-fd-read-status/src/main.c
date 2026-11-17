#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <mqueue.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#define QUEUE_NAME "/mqueue_fd_read_status"

static int check_shared_inode(int descriptor)
{
    const char path[] = "/dev/mqueue" QUEUE_NAME;
    struct stat before, by_path, by_fd;
    int failures = 0;

    if (fstat(descriptor, &before) != 0 || stat(path, &by_path) != 0
        || before.st_dev != by_path.st_dev || before.st_ino != by_path.st_ino) {
        fputs("mqd and pathname do not name the same inode\n", stderr);
        failures++;
    }
    if (syscall(SYS_fchmodat, AT_FDCWD, path, 0400) != 0
        || fstat(descriptor, &by_fd) != 0 || (by_fd.st_mode & 0777) != 0400) {
        fputs("pathname chmod was not visible through the retained mqd\n", stderr);
        failures++;
    }
    if (syscall(SYS_fchmod, descriptor, 0640) != 0 || stat(path, &by_path) != 0
        || (by_path.st_mode & 0777) != 0640) {
        fputs("mqd chmod was not visible through a new pathname lookup\n", stderr);
        failures++;
    }
    if (fstat(descriptor, &by_fd) != 0 || by_fd.st_ino != before.st_ino
        || by_fd.st_dev != before.st_dev || by_fd.st_size != 80) {
        fputs("metadata changes replaced the queue inode or its fixed size\n", stderr);
        failures++;
    }
    return failures == 0 ? 0 : -1;
}

int main(void)
{
    const char message[] = "abc";
    struct mq_attr attr = {
        .mq_maxmsg = 4,
        .mq_msgsize = 16,
    };
    mqd_t queue = (mqd_t)-1;
    mqd_t reader = (mqd_t)-1;
    mqd_t write_only = (mqd_t)-1;
    char status[128] = { 0 };
    const char expected[] =
        "QSIZE:3          NOTIFY:0     SIGNO:0     NOTIFY_PID:0     \n";
    int result = 1;

    queue = mq_open(QUEUE_NAME, O_CREAT | O_EXCL | O_RDWR, 0600, &attr);
    if (queue == (mqd_t)-1) {
        perror("mq_open");
        goto cleanup;
    }
    if (mq_send(queue, message, sizeof(message) - 1, 0) != 0) {
        perror("mq_send");
        goto cleanup;
    }

    ssize_t status_len = read((int)queue, status, 8);
    if (status_len != 8) {
        fprintf(stderr, "short status read returned %zd (errno=%d)\n", status_len, errno);
        goto cleanup;
    }
    status_len = read((int)queue, status + 8, sizeof(status) - 1 - 8);
    if (status_len <= 0) {
        fprintf(stderr, "status remainder read returned %zd (errno=%d)\n", status_len, errno);
        goto cleanup;
    }
    status[8 + status_len] = '\0';
    if (strcmp(status, expected) != 0) {
        fprintf(stderr, "unexpected mqueue status: %s\n", status);
        goto cleanup;
    }

    if (read((int)queue, status, sizeof(status)) != 0) {
        fputs("mqueue status fd did not advance to EOF\n", stderr);
        goto cleanup;
    }

    reader = mq_open(QUEUE_NAME, O_RDONLY);
    if (reader == (mqd_t)-1) {
        perror("mq_open(O_RDONLY)");
        goto cleanup;
    }
    status_len = read((int)reader, status, sizeof(status) - 1);
    if (status_len != (ssize_t)strlen(expected)
        || memcmp(status, expected, strlen(expected)) != 0) {
        fputs("separate mqd_t did not start at status offset zero\n", stderr);
        goto cleanup;
    }

    if (lseek((int)queue, 0, SEEK_SET) != 0) {
        perror("lseek(mqd_t, SEEK_SET)");
        goto cleanup;
    }
    status_len = read((int)queue, status, sizeof(status) - 1);
    if (status_len != (ssize_t)strlen(expected)
        || memcmp(status, expected, strlen(expected)) != 0) {
        fputs("mqueue status fd was not reset by lseek\n", stderr);
        goto cleanup;
    }

    write_only = mq_open(QUEUE_NAME, O_WRONLY);
    if (write_only == (mqd_t)-1) {
        perror("mq_open(O_WRONLY)");
        goto cleanup;
    }
    errno = 0;
    if (read((int)write_only, status, sizeof(status)) != -1 || errno != EBADF) {
        fprintf(stderr, "read(O_WRONLY mqd_t) did not return EBADF (errno=%d)\n", errno);
        goto cleanup;
    }

    result = check_shared_inode((int)queue) == 0 ? 0 : 1;

cleanup:
    if (write_only != (mqd_t)-1) {
        mq_close(write_only);
    }
    if (reader != (mqd_t)-1) {
        mq_close(reader);
    }
    if (queue != (mqd_t)-1) {
        mq_close(queue);
    }
    mq_unlink(QUEUE_NAME);
    return result;
}
