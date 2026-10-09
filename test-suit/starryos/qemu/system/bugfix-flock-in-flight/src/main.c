#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/file.h>
#include <sys/socket.h>
#include <sys/uio.h>
#include <unistd.h>

static int passed;
static int failed;

static void check(int condition, const char *message)
{
    if (condition) {
        ++passed;
        printf("PASS: %s\n", message);
    } else {
        ++failed;
        printf("FAIL: %s\n", message);
    }
}

static const char *path = "/tmp/flock-in-flight";

static int contended(void)
{
    int probe = open(path, O_RDWR);
    if (probe < 0) {
        return -1;
    }
    int result = flock(probe, LOCK_EX | LOCK_NB);
    int saved = errno;
    close(probe);
    if (result == 0) {
        return 0;
    }
    return saved == EWOULDBLOCK ? 1 : -1;
}

static int send_fd(int sock, int fd)
{
    char byte = 'f';
    struct iovec iov = {.iov_base = &byte, .iov_len = 1};
    union {
        struct cmsghdr header;
        char space[CMSG_SPACE(sizeof(int))];
    } control;
    struct msghdr msg;
    memset(&msg, 0, sizeof(msg));
    memset(&control, 0, sizeof(control));
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.space;
    msg.msg_controllen = sizeof(control.space);
    struct cmsghdr *cmsg = CMSG_FIRSTHDR(&msg);
    cmsg->cmsg_level = SOL_SOCKET;
    cmsg->cmsg_type = SCM_RIGHTS;
    cmsg->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(cmsg), &fd, sizeof(int));
    return sendmsg(sock, &msg, 0) == 1;
}

static int receive_fd(int sock)
{
    char byte;
    struct iovec iov = {.iov_base = &byte, .iov_len = 1};
    union {
        struct cmsghdr header;
        char space[CMSG_SPACE(sizeof(int))];
    } control;
    struct msghdr msg;
    memset(&msg, 0, sizeof(msg));
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.space;
    msg.msg_controllen = sizeof(control.space);
    if (recvmsg(sock, &msg, 0) != 1) {
        return -1;
    }
    struct cmsghdr *cmsg = CMSG_FIRSTHDR(&msg);
    if (cmsg == NULL || cmsg->cmsg_type != SCM_RIGHTS) {
        return -1;
    }
    int fd;
    memcpy(&fd, CMSG_DATA(cmsg), sizeof(int));
    return fd;
}

int main(void)
{
    int pair[2];
    check(socketpair(AF_UNIX, SOCK_STREAM, 0, pair) == 0, "create a unix socket pair");

    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0600);
    check(fd >= 0 && flock(fd, LOCK_EX) == 0, "take an exclusive flock");
    check(send_fd(pair[0], fd), "queue the only descriptor in an SCM_RIGHTS message");
    close(fd);

    /* The queued message still refers to the open file description, and a
     * flock belongs to the description until its last reference is gone. */
    check(contended() == 1, "the lock holds while the description is in flight");

    int received = receive_fd(pair[1]);
    check(received >= 0, "receive the descriptor");
    check(contended() == 1, "the lock holds once the descriptor is installed again");
    close(received);
    check(contended() == 0, "closing the last reference releases the lock");

    close(pair[0]);
    close(pair[1]);
    unlink(path);
    printf("RESULT: %d passed / %d failed\n", passed, failed);
    if (failed == 0) {
        printf("TEST PASSED\n");
        return 0;
    }
    printf("TEST FAILED\n");
    return 1;
}
