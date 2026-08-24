#define _GNU_SOURCE

/*
 * End-to-end socket regression for the queue-level network poll runtime.
 *
 * The cases intentionally use real AF_INET loopback TCP sockets rather than a
 * mocked device. Together they pin the Linux-visible behavior that can regress
 * when IRQ/queue ownership or the single protocol executor loses a wakeup:
 * blocking and nonblocking connect/accept, send/recv, poll/epoll readiness,
 * peer close, and EINTR from a signal without SA_RESTART.
 */

#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

#define EVENT_TIMEOUT_MS 2000
#define CHILD_TIMEOUT_MS 5000
#define WAIT_STEP_US 10000

static volatile sig_atomic_t got_usr1;

static void on_usr1(int signo)
{
    if (signo == SIGUSR1) {
        got_usr1 = 1;
    }
}

static int close_fd(int fd)
{
    return fd >= 0 ? close(fd) : 0;
}

static int create_listener(struct sockaddr_in *address)
{
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) {
        return -1;
    }

    int reuse = 1;
    if (setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &reuse, sizeof(reuse)) < 0) {
        close(fd);
        return -1;
    }

    memset(address, 0, sizeof(*address));
    address->sin_family = AF_INET;
    address->sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    address->sin_port = 0;
    if (bind(fd, (struct sockaddr *)address, sizeof(*address)) < 0 ||
        listen(fd, 8) < 0) {
        close(fd);
        return -1;
    }

    socklen_t length = sizeof(*address);
    if (getsockname(fd, (struct sockaddr *)address, &length) < 0) {
        close(fd);
        return -1;
    }
    return fd;
}

static int create_tcp_pair(int *client, int *server)
{
    struct sockaddr_in address;
    int listener = create_listener(&address);
    if (listener < 0) {
        return -1;
    }

    *client = socket(AF_INET, SOCK_STREAM, 0);
    if (*client < 0 ||
        connect(*client, (struct sockaddr *)&address, sizeof(address)) < 0) {
        close_fd(*client);
        close(listener);
        return -1;
    }

    *server = accept(listener, NULL, NULL);
    close(listener);
    if (*server < 0) {
        close(*client);
        return -1;
    }
    return 0;
}

static int epoll_wait_retry(int epfd, struct epoll_event *event, int timeout_ms)
{
    for (;;) {
        int result = epoll_wait(epfd, event, 1, timeout_ms);
        if (result < 0 && errno == EINTR) {
            continue;
        }
        return result;
    }
}

static int wait_child(pid_t child)
{
    int status = 0;
    for (int waited = 0; waited < CHILD_TIMEOUT_MS;
         waited += WAIT_STEP_US / 1000) {
        pid_t result = waitpid(child, &status, WNOHANG);
        if (result == child) {
            return WIFEXITED(status) && WEXITSTATUS(status) == 0 ? 0 : -1;
        }
        if (result < 0 && errno != EINTR) {
            return -1;
        }
        usleep(WAIT_STEP_US);
    }
    kill(child, SIGKILL);
    waitpid(child, NULL, 0);
    return -1;
}

static int read_ready(int fd)
{
    char byte = 0;
    for (;;) {
        ssize_t count = read(fd, &byte, 1);
        if (count == 1) {
            return byte == 'R' ? 0 : -1;
        }
        if (count < 0 && errno == EINTR) {
            continue;
        }
        return -1;
    }
}

static int test_blocking_data_and_epoll(void)
{
    int client = -1;
    int server = -1;
    int epfd = -1;
    int result = -1;
    const char payload[] = "queue-napi";
    char received[sizeof(payload)] = {0};

    if (create_tcp_pair(&client, &server) < 0) {
        perror("blocking: create_tcp_pair");
        goto out;
    }
    epfd = epoll_create1(EPOLL_CLOEXEC);
    struct epoll_event interest = {
        .events = EPOLLIN,
        .data.fd = client,
    };
    if (epfd < 0 || epoll_ctl(epfd, EPOLL_CTL_ADD, client, &interest) < 0) {
        perror("blocking: epoll setup");
        goto out;
    }
    if (send(server, payload, sizeof(payload), 0) != (ssize_t)sizeof(payload)) {
        perror("blocking: send");
        goto out;
    }

    struct epoll_event event;
    if (epoll_wait_retry(epfd, &event, EVENT_TIMEOUT_MS) != 1 ||
        !(event.events & EPOLLIN)) {
        fprintf(stderr, "blocking: missing EPOLLIN\n");
        goto out;
    }
    if (recv(client, received, sizeof(received), MSG_WAITALL) !=
            (ssize_t)sizeof(received) ||
        memcmp(received, payload, sizeof(payload)) != 0) {
        fprintf(stderr, "blocking: recv payload mismatch\n");
        goto out;
    }
    result = 0;

out:
    close_fd(epfd);
    close_fd(client);
    close_fd(server);
    return result;
}

static int test_nonblocking_connect_accept_and_poll(void)
{
    struct sockaddr_in address;
    int listener = -1;
    int client = -1;
    int server = -1;
    int epfd = -1;
    int result = -1;

    listener = create_listener(&address);
    client = socket(AF_INET, SOCK_STREAM | SOCK_NONBLOCK, 0);
    if (listener < 0 || client < 0) {
        perror("nonblock: setup");
        goto out;
    }

    int connect_result = connect(client, (struct sockaddr *)&address,
                                 sizeof(address));
    if (connect_result < 0 && errno != EINPROGRESS) {
        perror("nonblock: connect");
        goto out;
    }

    epfd = epoll_create1(EPOLL_CLOEXEC);
    struct epoll_event interest = {
        .events = EPOLLOUT | EPOLLERR,
        .data.fd = client,
    };
    if (epfd < 0 || epoll_ctl(epfd, EPOLL_CTL_ADD, client, &interest) < 0) {
        perror("nonblock: epoll setup");
        goto out;
    }
    struct epoll_event event;
    if (epoll_wait_retry(epfd, &event, EVENT_TIMEOUT_MS) != 1 ||
        !(event.events & (EPOLLOUT | EPOLLERR))) {
        fprintf(stderr, "nonblock: connect completion did not wake epoll\n");
        goto out;
    }

    int socket_error = -1;
    socklen_t error_len = sizeof(socket_error);
    if (getsockopt(client, SOL_SOCKET, SO_ERROR, &socket_error, &error_len) < 0 ||
        socket_error != 0) {
        fprintf(stderr, "nonblock: SO_ERROR=%d\n", socket_error);
        goto out;
    }

    server = accept(listener, NULL, NULL);
    if (server < 0 || send(client, "P", 1, 0) != 1) {
        perror("nonblock: accept/send");
        goto out;
    }
    struct pollfd pollfd = {
        .fd = server,
        .events = POLLIN,
    };
    char byte = 0;
    if (poll(&pollfd, 1, EVENT_TIMEOUT_MS) != 1 ||
        !(pollfd.revents & POLLIN) || recv(server, &byte, 1, 0) != 1 ||
        byte != 'P') {
        fprintf(stderr, "nonblock: poll/recv mismatch\n");
        goto out;
    }
    result = 0;

out:
    close_fd(epfd);
    close_fd(client);
    close_fd(server);
    close_fd(listener);
    return result;
}

static int test_signal_interrupts_blocking_recv(void)
{
    int client = -1;
    int server = -1;
    int ready[2] = {-1, -1};
    if (create_tcp_pair(&client, &server) < 0 || pipe(ready) < 0) {
        perror("signal: setup");
        return -1;
    }

    pid_t child = fork();
    if (child < 0) {
        perror("signal: fork");
        return -1;
    }
    if (child == 0) {
        close(client);
        close(ready[0]);
        struct sigaction action;
        memset(&action, 0, sizeof(action));
        action.sa_handler = on_usr1;
        sigemptyset(&action.sa_mask);
        if (sigaction(SIGUSR1, &action, NULL) < 0 ||
            write(ready[1], "R", 1) != 1) {
            _exit(1);
        }
        char byte;
        errno = 0;
        ssize_t count = recv(server, &byte, 1, 0);
        int saved_errno = errno;
        _exit(count == -1 && saved_errno == EINTR && got_usr1 ? 0 : 1);
    }

    close(server);
    close(ready[1]);
    int result = read_ready(ready[0]);
    if (result == 0 && kill(child, SIGUSR1) < 0) {
        result = -1;
    }
    if (result == 0) {
        result = wait_child(child);
    } else {
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
    }
    close(ready[0]);
    close(client);
    return result;
}

static int test_peer_close_wakes_epoll(void)
{
    int client = -1;
    int server = -1;
    int ready[2] = {-1, -1};
    if (create_tcp_pair(&client, &server) < 0 || pipe(ready) < 0) {
        perror("close: setup");
        return -1;
    }

    pid_t child = fork();
    if (child < 0) {
        perror("close: fork");
        return -1;
    }
    if (child == 0) {
        close(client);
        close(ready[0]);
        int epfd = epoll_create1(EPOLL_CLOEXEC);
        struct epoll_event interest = {
            .events = EPOLLIN | EPOLLRDHUP | EPOLLHUP,
            .data.fd = server,
        };
        if (epfd < 0 || epoll_ctl(epfd, EPOLL_CTL_ADD, server, &interest) < 0 ||
            write(ready[1], "R", 1) != 1) {
            _exit(1);
        }
        struct epoll_event event;
        int count = epoll_wait_retry(epfd, &event, EVENT_TIMEOUT_MS);
        char byte;
        ssize_t received = recv(server, &byte, 1, 0);
        _exit(count == 1 &&
                      (event.events & (EPOLLIN | EPOLLRDHUP | EPOLLHUP)) &&
                      received == 0
                  ? 0
                  : 1);
    }

    close(server);
    close(ready[1]);
    int result = read_ready(ready[0]);
    close(client);
    if (result == 0) {
        result = wait_child(child);
    } else {
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
    }
    close(ready[0]);
    return result;
}

int main(void)
{
    if (test_blocking_data_and_epoll() < 0 ||
        test_nonblocking_connect_accept_and_poll() < 0 ||
        test_signal_interrupts_blocking_recv() < 0 ||
        test_peer_close_wakes_epoll() < 0) {
        fprintf(stderr, "STARRY_GROUPED_TEST_FAILED: test-tcp-napi-runtime\n");
        return 1;
    }

    printf("STARRY_GROUPED_TEST_PASSED: test-tcp-napi-runtime\n");
    return 0;
}
