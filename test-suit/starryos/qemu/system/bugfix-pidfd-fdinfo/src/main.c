#define _GNU_SOURCE

/*
 * Linux exposes the process referenced by a pidfd through the Pid and NSpid
 * fields in /proc/self/fdinfo/<fd>. systemd uses the Pid field immediately
 * after pidfd_spawn() to construct a race-free process reference.
 */
#include <ctype.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdbool.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef CLONE_PIDFD
#define CLONE_PIDFD 0x00001000ULL
#endif

#ifndef CLONE_NEWPID
#define CLONE_NEWPID 0x20000000ULL
#endif

#ifndef PIDFD_THREAD
#define PIDFD_THREAD O_EXCL
#endif

struct clone_args {
    unsigned long long flags;
    unsigned long long pidfd;
    unsigned long long child_tid;
    unsigned long long parent_tid;
    unsigned long long exit_signal;
    unsigned long long stack;
    unsigned long long stack_size;
    unsigned long long tls;
    unsigned long long set_tid;
    unsigned long long set_tid_size;
    unsigned long long cgroup;
};

static int read_pid_field(FILE *stream, const char *field, pid_t *value)
{
    char line[128];
    size_t field_length = strlen(field);

    rewind(stream);
    while (fgets(line, sizeof(line), stream) != NULL) {
        if (strncmp(line, field, field_length) != 0 || line[field_length] != ':')
            continue;

        char *end = NULL;
        errno = 0;
        long parsed = strtol(line + field_length + 1, &end, 10);
        if (errno != 0 || end == line + field_length + 1 || parsed <= 0)
            return -1;

        *value = (pid_t)parsed;
        return 0;
    }

    return -1;
}

static int read_nspid(FILE *stream, pid_t *outer_pid, pid_t *inner_pid)
{
    char line[128];

    rewind(stream);
    while (fgets(line, sizeof(line), stream) != NULL) {
        if (strncmp(line, "NSpid:", 6) != 0)
            continue;

        char *end = NULL;
        errno = 0;
        long outer = strtol(line + 6, &end, 10);
        if (errno != 0 || end == line + 6 || outer <= 0)
            return -1;
        long inner = strtol(end, &end, 10);
        if (errno != 0 || inner <= 0)
            return -1;
        while (isspace((unsigned char)*end))
            end++;
        if (*end != '\0')
            return -1;

        *outer_pid = (pid_t)outer;
        *inner_pid = (pid_t)inner;
        return 0;
    }

    return -1;
}

static int pidfd_fdinfo_reports_reaped_process(FILE *stream)
{
    char line[128];
    bool pid_is_reaped = false;
    bool nspid_is_reaped = false;

    rewind(stream);
    while (fgets(line, sizeof(line), stream) != NULL) {
        pid_is_reaped |= strcmp(line, "Pid:\t-1\n") == 0;
        nspid_is_reaped |= strcmp(line, "NSpid:\t-1\n") == 0;
    }

    return pid_is_reaped && nspid_is_reaped ? 0 : -1;
}

static int pidfd_is_hidden_in_fdinfo(int pidfd)
{
    char path[64];
    snprintf(path, sizeof(path), "/proc/self/fdinfo/%d", pidfd);
    FILE *stream = fopen(path, "re");
    if (stream == NULL)
        return -1;

    bool pid_is_zero = false;
    bool nspid_is_zero = false;
    char line[128];
    while (fgets(line, sizeof(line), stream) != NULL) {
        pid_is_zero |= strcmp(line, "Pid:\t0\n") == 0;
        nspid_is_zero |= strcmp(line, "NSpid:\t0\n") == 0;
    }
    fclose(stream);
    return pid_is_zero && nspid_is_zero ? 0 : -1;
}

struct received_pidfd {
    int fd;
    pid_t tid;
};

static int send_pidfd(int socket, int fd, pid_t tid)
{
    struct iovec iov = { .iov_base = &tid, .iov_len = sizeof(tid) };
    char control[CMSG_SPACE(sizeof(fd))] = {0};
    struct msghdr message = {
        .msg_iov = &iov,
        .msg_iovlen = 1,
        .msg_control = control,
        .msg_controllen = sizeof(control),
    };
    struct cmsghdr *cmsg = CMSG_FIRSTHDR(&message);
    cmsg->cmsg_level = SOL_SOCKET;
    cmsg->cmsg_type = SCM_RIGHTS;
    cmsg->cmsg_len = CMSG_LEN(sizeof(fd));
    memcpy(CMSG_DATA(cmsg), &fd, sizeof(fd));
    return sendmsg(socket, &message, 0) == (ssize_t)sizeof(tid) ? 0 : -1;
}

static struct received_pidfd recv_pidfd(int socket)
{
    pid_t tid = -1;
    struct iovec iov = { .iov_base = &tid, .iov_len = sizeof(tid) };
    char control[CMSG_SPACE(sizeof(int))] = {0};
    struct msghdr message = {
        .msg_iov = &iov,
        .msg_iovlen = 1,
        .msg_control = control,
        .msg_controllen = sizeof(control),
    };
    if (recvmsg(socket, &message, 0) != (ssize_t)sizeof(tid) ||
        (message.msg_flags & MSG_CTRUNC) != 0)
        return (struct received_pidfd){ .fd = -1, .tid = -1 };

    struct cmsghdr *cmsg = CMSG_FIRSTHDR(&message);
    if (cmsg == NULL || cmsg->cmsg_level != SOL_SOCKET ||
        cmsg->cmsg_type != SCM_RIGHTS || cmsg->cmsg_len != CMSG_LEN(sizeof(int)))
        return (struct received_pidfd){ .fd = -1, .tid = -1 };

    int fd = -1;
    memcpy(&fd, CMSG_DATA(cmsg), sizeof(fd));
    return (struct received_pidfd){ .fd = fd, .tid = tid };
}

static void *publish_thread_pidfd(void *arg)
{
    int socket = *(int *)arg;
    pid_t tid = (pid_t)syscall(SYS_gettid);
    int pidfd = (int)syscall(SYS_pidfd_open, tid, PIDFD_THREAD);
    if (pidfd < 0 || send_pidfd(socket, pidfd, tid) != 0) {
        fprintf(stderr,
                "FAIL: publish PIDFD_THREAD tid=%d pidfd=%d errno=%d (%s)\n",
                tid, pidfd, errno, strerror(errno));
        if (pidfd >= 0)
            close(pidfd);
        return (void *)1;
    }

    close(pidfd);
    for (;;)
        pause();
}

static int check_thread_pidfd_fdinfo(struct received_pidfd thread_pidfd)
{
    if (thread_pidfd.fd < 0 || thread_pidfd.tid <= 0)
        return -1;

    char path[64];
    snprintf(path, sizeof(path), "/proc/self/fdinfo/%d", thread_pidfd.fd);
    FILE *stream = fopen(path, "re");
    if (stream == NULL)
        return -1;

    pid_t reported_pid = -1;
    pid_t reported_nspid_outer = -1;
    pid_t reported_nspid_inner = -1;
    int pid_result = read_pid_field(stream, "Pid", &reported_pid);
    int nspid_result = read_nspid(stream, &reported_nspid_outer,
                                  &reported_nspid_inner);
    fclose(stream);

    if (pid_result != 0 || reported_pid != thread_pidfd.tid ||
        nspid_result != 0 || reported_nspid_outer != thread_pidfd.tid ||
        reported_nspid_inner != 2) {
        fprintf(stderr,
                "FAIL: PIDFD_THREAD fdinfo Pid=%d NSpid=%d %d expected=%d %d 2\n",
                reported_pid, reported_nspid_outer, reported_nspid_inner,
                thread_pidfd.tid, thread_pidfd.tid);
        return -1;
    }
    return 0;
}

static int check_transferred_pidfd_in_child_pid_namespace(int socket)
{
    if (unshare(CLONE_NEWPID) != 0) {
        fprintf(stderr, "FAIL: unshare(CLONE_NEWPID): errno=%d (%s)\n", errno,
                strerror(errno));
        return 1;
    }

    pid_t namespace_init = fork();
    if (namespace_init < 0) {
        fprintf(stderr, "FAIL: fork PID namespace init: errno=%d (%s)\n", errno,
                strerror(errno));
        return 1;
    }
    if (namespace_init == 0) {
        struct received_pidfd received = recv_pidfd(socket);
        int result = received.fd >= 0 ? pidfd_is_hidden_in_fdinfo(received.fd) : -1;
        if (received.fd >= 0)
            close(received.fd);
        close(socket);
        if (result != 0)
            fprintf(stderr, "FAIL: transferred pidfd is visible in child PID namespace\n");
        return result == 0 ? 0 : 1;
    }

    close(socket);
    int status = 0;
    if (waitpid(namespace_init, &status, 0) != namespace_init)
        return 1;
    return WIFEXITED(status) && WEXITSTATUS(status) == 0 ? 0 : 1;
}

static int check_pidfd_transfer_to_child_pid_namespace(int pidfd)
{
    int sockets[2];
    if (socketpair(AF_UNIX, SOCK_SEQPACKET, 0, sockets) != 0) {
        fprintf(stderr, "FAIL: socketpair: errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }

    pid_t receiver = fork();
    if (receiver < 0) {
        fprintf(stderr, "FAIL: fork pidfd receiver: errno=%d (%s)\n", errno,
                strerror(errno));
        close(sockets[0]);
        close(sockets[1]);
        return 1;
    }
    if (receiver == 0) {
        close(sockets[0]);
        _exit(check_transferred_pidfd_in_child_pid_namespace(sockets[1]));
    }

    close(sockets[1]);
    int sent = send_pidfd(sockets[0], pidfd, 0);
    close(sockets[0]);
    int status = 0;
    if (waitpid(receiver, &status, 0) != receiver)
        return 1;
    return sent == 0 && WIFEXITED(status) && WEXITSTATUS(status) == 0 ? 0 : 1;
}

int main(void)
{
    int thread_sockets[2];
    if (socketpair(AF_UNIX, SOCK_SEQPACKET, 0, thread_sockets) != 0) {
        fprintf(stderr, "FAIL: thread socketpair: errno=%d (%s)\n", errno,
                strerror(errno));
        return 1;
    }

    int pidfd = -1;
    struct clone_args args = {
        .flags = CLONE_PIDFD | CLONE_NEWPID,
        .pidfd = (unsigned long long)&pidfd,
        .exit_signal = SIGCHLD,
    };

    pid_t child = (pid_t)syscall(SYS_clone3, &args, sizeof(args));
    if (child == 0) {
        close(thread_sockets[0]);
        pthread_t thread;
        int err = pthread_create(&thread, NULL, publish_thread_pidfd,
                                 &thread_sockets[1]);
        if (err != 0) {
            fprintf(stderr, "FAIL: pthread_create: errno=%d (%s)\n", err,
                    strerror(err));
            return 1;
        }
        for (;;)
            pause();
    }
    close(thread_sockets[1]);
    if (child < 0 || pidfd < 0) {
        fprintf(stderr,
                "FAIL: clone3(CLONE_PIDFD | CLONE_NEWPID): errno=%d (%s) pidfd=%d\n",
                errno, strerror(errno), pidfd);
        return 1;
    }

    struct received_pidfd thread_pidfd = recv_pidfd(thread_sockets[0]);
    close(thread_sockets[0]);
    int thread_fdinfo_result = check_thread_pidfd_fdinfo(thread_pidfd);

    char path[64];
    snprintf(path, sizeof(path), "/proc/self/fdinfo/%d", pidfd);
    FILE *stream = fopen(path, "re");
    if (stream == NULL) {
        fprintf(stderr, "FAIL: open %s: errno=%d (%s)\n", path, errno,
                strerror(errno));
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
        close(pidfd);
        return 1;
    }

    pid_t reported_pid = -1;
    pid_t reported_nspid_outer = -1;
    pid_t reported_nspid_inner = -1;
    int pid_result = read_pid_field(stream, "Pid", &reported_pid);
    int nspid_result = read_nspid(stream, &reported_nspid_outer,
                                  &reported_nspid_inner);
    fclose(stream);

    int transfer_result = check_pidfd_transfer_to_child_pid_namespace(pidfd);
    kill(child, SIGKILL);
    int status = 0;
    int waited = waitpid(child, &status, 0);
    FILE *exited_stream = fopen(path, "re");
    int exited_fdinfo_result = exited_stream == NULL
                                   ? -1
                                   : pidfd_fdinfo_reports_reaped_process(exited_stream);
    if (exited_stream != NULL)
        fclose(exited_stream);
    if (thread_pidfd.fd >= 0)
        close(thread_pidfd.fd);
    close(pidfd);

    if (thread_fdinfo_result != 0)
        return 1;
    if (pid_result != 0 || reported_pid != child) {
        fprintf(stderr, "FAIL: pidfd fdinfo Pid=%d expected=%d\n",
                reported_pid, child);
        return 1;
    }
    if (nspid_result != 0 || reported_nspid_outer != child ||
        reported_nspid_inner != 1) {
        fprintf(stderr,
                "FAIL: pidfd fdinfo NSpid=%d %d expected=%d 1\n",
                reported_nspid_outer, reported_nspid_inner, child);
        return 1;
    }
    if (transfer_result != 0) {
        fprintf(stderr, "FAIL: transferred pidfd must report Pid/NSpid as 0\n");
        return 1;
    }
    if (waited != child || !WIFSIGNALED(status) || WTERMSIG(status) != SIGKILL) {
        fprintf(stderr, "FAIL: child cleanup status=%d waited=%d expected=%d\n",
                status, waited, child);
        return 1;
    }
    if (exited_fdinfo_result != 0) {
        fputs("FAIL: exited pidfd fdinfo must report Pid/NSpid as -1\n", stderr);
        return 1;
    }

    printf("PASS: pidfd fdinfo Pid=%d NSpid=%d %d\n", reported_pid,
           reported_nspid_outer, reported_nspid_inner);
    puts("STARRY_PIDFD_FDINFO_PASSED");
    return 0;
}
