#include <errno.h>
#include <fcntl.h>
#include <mqueue.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#define QUEUE_NAME "/mqueue_fd_read_status"

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

    result = 0;

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
