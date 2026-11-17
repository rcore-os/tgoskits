#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <mqueue.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#define QUEUE_NAME "/mqueue_fd_read_status"

static int check_readonly_path(void)
{
    const char directory[] = "/tmp/mqueue-fd-readonly";
    const char path[] = "/tmp/mqueue-fd-readonly" QUEUE_NAME;
    int descriptor = -1;
    int mounted = 0;
    int result = -1;

    if (mkdir(directory, 0700) != 0
        || syscall(SYS_mount, "/dev/mqueue", directory, NULL, MS_BIND, NULL) != 0) {
        perror("prepare readonly mqueue view");
        goto cleanup;
    }
    mounted = 1;
    if (syscall(SYS_mount, NULL, directory, NULL,
                MS_REMOUNT | MS_BIND | MS_RDONLY, NULL) != 0) {
        perror("remount mqueue bind readonly");
        goto cleanup;
    }
    descriptor = (int)syscall(SYS_openat, AT_FDCWD, path, O_RDONLY, 0);
    if (descriptor < 0) {
        perror("open readonly mqueue path");
        goto cleanup;
    }
    errno = 0;
    if (syscall(SYS_fchmod, descriptor, 0600) != -1 || errno != EROFS) {
        fprintf(stderr, "mqueue descriptor lost its readonly mount (errno=%d)\n", errno);
        goto cleanup;
    }
    result = 0;

cleanup:
    if (descriptor >= 0) close(descriptor);
    if (mounted) syscall(SYS_umount2, directory, MNT_DETACH);
    rmdir(directory);
    return result;
}

static int check_path_descriptor(int queue, const char *expected)
{
    const char path[] = "/dev/mqueue" QUEUE_NAME;
    const char payload[] = "path";
    char status[128] = { 0 };
    char received[32] = { 0 };
    unsigned int priority = 0;
    struct mq_attr attributes = { 0 };
    int failures = 0;
    int duplicate = -1;
    int descriptor = (int)syscall(SYS_openat, AT_FDCWD, path, O_RDWR | O_NONBLOCK, 0);
    if (descriptor < 0) {
        perror("open mqueue pathname");
        return -1;
    }

    if (syscall(SYS_fcntl, descriptor, F_GETFD) != 0
        || (syscall(SYS_fcntl, queue, F_GETFD) & FD_CLOEXEC) == 0) {
        fputs("pathname and mq_open descriptors lost their distinct CLOEXEC rules\n", stderr);
        failures++;
    }
    if (syscall(SYS_mq_getsetattr, descriptor, NULL, &attributes) != 0
        || (attributes.mq_flags & O_NONBLOCK) == 0 || attributes.mq_curmsgs != 1) {
        fputs("pathname fd did not expose its real message queue\n", stderr);
        failures++;
    }

    if (read(descriptor, status, 8) != 8 || (duplicate = dup(descriptor)) < 0) {
        fputs("prepare shared pathname status offset\n", stderr);
        failures++;
    } else {
        ssize_t length = read(duplicate, status + 8, sizeof(status) - 9);
        if (length <= 0 || strcmp(status, expected) != 0) {
            fputs("duplicated pathname fd did not share the status offset\n", stderr);
            failures++;
        }
        attributes.mq_flags = 0;
        if (syscall(SYS_mq_getsetattr, duplicate, &attributes, NULL) != 0
            || syscall(SYS_mq_getsetattr, descriptor, NULL, &attributes) != 0
            || (attributes.mq_flags & O_NONBLOCK) != 0
            || syscall(SYS_mq_getsetattr, queue, NULL, &attributes) != 0
            || (attributes.mq_flags & O_NONBLOCK) != 0) {
            fputs("mq_setattr did not preserve shared and independent descriptions\n", stderr);
            failures++;
        }
    }

    if (syscall(SYS_mq_timedsend, descriptor, payload, sizeof(payload) - 1, 5, NULL) != 0
        || syscall(SYS_mq_timedreceive, queue, received, sizeof(received), &priority, NULL)
            != (long)sizeof(payload) - 1
        || priority != 5 || memcmp(received, payload, sizeof(payload) - 1) != 0) {
        fputs("pathname and mq_open descriptors did not operate on the same queue\n", stderr);
        failures++;
    }
    errno = 0;
    if (syscall(SYS_write, descriptor, "X", 1) != -1 || errno != EINVAL) {
        fprintf(stderr, "pathname queue write did not return EINVAL (errno=%d)\n", errno);
        failures++;
    }

    struct sigevent notification = { .sigev_notify = SIGEV_NONE };
    if (duplicate >= 0) {
        if (syscall(SYS_mq_notify, descriptor, &notification) != 0) {
            fputs("register notification through pathname fd\n", stderr);
            failures++;
        }
        close(duplicate);
        duplicate = -1;
        if (syscall(SYS_mq_notify, queue, &notification) != 0) {
            fputs("closing a pathname duplicate did not flush its notification\n", stderr);
            failures++;
        }
        syscall(SYS_mq_notify, queue, NULL);
    }

    if (lseek(descriptor, 0, SEEK_END) != 80) {
        fputs("pathname SEEK_END did not use the fixed inode size\n", stderr);
        failures++;
    }
    const struct timespec times[2] = {
        { .tv_sec = 123456, .tv_nsec = 123 },
        { .tv_sec = 654321, .tv_nsec = 456 },
    };
    struct stat before, after;
    if (syscall(SYS_utimensat, descriptor, NULL, times, 0) != 0
        || fstat(descriptor, &before) != 0 || read(descriptor, status, 1) != 0
        || fstat(descriptor, &after) != 0
        || before.st_atim.tv_sec != after.st_atim.tv_sec
        || before.st_atim.tv_nsec != after.st_atim.tv_nsec
        || before.st_ctim.tv_sec != after.st_ctim.tv_sec
        || before.st_ctim.tv_nsec != after.st_ctim.tv_nsec) {
        fputs("EOF pathname status read changed inode timestamps\n", stderr);
        failures++;
    }
    if (check_readonly_path() != 0) failures++;

    if (duplicate >= 0) close(duplicate);
    close(descriptor);
    return failures == 0 ? 0 : -1;
}

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

    int path_result = check_path_descriptor((int)queue, expected);
    int inode_result = check_shared_inode((int)queue);
    result = path_result == 0 && inode_result == 0 ? 0 : 1;

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
