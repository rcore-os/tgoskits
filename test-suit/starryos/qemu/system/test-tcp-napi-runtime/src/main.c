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
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
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

static long raw_poll_wait(struct pollfd *fds, nfds_t nfds, int timeout_ms)
{
#ifdef SYS_poll
    return syscall(SYS_poll, fds, nfds, timeout_ms);
#elif defined(SYS_ppoll)
    struct timespec timeout;
    struct timespec *timeout_ptr = NULL;
    if (timeout_ms >= 0) {
        timeout.tv_sec = timeout_ms / 1000;
        timeout.tv_nsec = (timeout_ms % 1000) * 1000000L;
        timeout_ptr = &timeout;
    }
    return syscall(SYS_ppoll, fds, nfds, timeout_ptr, NULL, 0);
#else
#error "A raw poll syscall is required"
#endif
}

static long raw_epoll_wait(int epfd, struct epoll_event *event,
                           int timeout_ms)
{
#ifdef SYS_epoll_wait
    return syscall(SYS_epoll_wait, epfd, event, 1, timeout_ms);
#elif defined(SYS_epoll_pwait)
    return syscall(SYS_epoll_pwait, epfd, event, 1, timeout_ms, NULL, 0);
#else
#error "A raw epoll wait syscall is required"
#endif
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

    int listener_flags = fcntl(listener, F_GETFL);
    if (listener_flags < 0 ||
        fcntl(listener, F_SETFL, listener_flags | O_NONBLOCK) < 0) {
        perror("nonblock: listener flags");
        goto out;
    }
    errno = 0;
    if (syscall(SYS_accept4, listener, NULL, NULL, 0) != -1 ||
        (errno != EAGAIN && errno != EWOULDBLOCK)) {
        fprintf(stderr, "nonblock: empty accept4 did not return EAGAIN\n");
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

    server = syscall(SYS_accept4, listener, NULL, NULL,
                     SOCK_NONBLOCK | SOCK_CLOEXEC);
    if (server < 0 || !(fcntl(server, F_GETFL) & O_NONBLOCK) ||
        !(fcntl(server, F_GETFD) & FD_CLOEXEC)) {
        perror("nonblock: accept4 flags");
        goto out;
    }
    char byte = 0;
    errno = 0;
    if (recv(server, &byte, 1, 0) != -1 ||
        (errno != EAGAIN && errno != EWOULDBLOCK)) {
        fprintf(stderr, "nonblock: empty recv did not return EAGAIN\n");
        goto out;
    }
    if (send(client, "P", 1, 0) != 1) {
        perror("nonblock: accept/send");
        goto out;
    }
    struct pollfd pollfd = {
        .fd = server,
        .events = POLLIN,
    };
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

static int test_blocking_accept_wakeup(void)
{
    struct sockaddr_in address;
    int listener = create_listener(&address);
    int ready[2] = {-1, -1};
    if (listener < 0 || pipe(ready) < 0) {
        perror("accept-wake: setup");
        close_fd(listener);
        return -1;
    }

    pid_t child = fork();
    if (child < 0) {
        perror("accept-wake: fork");
        close(listener);
        close(ready[0]);
        close(ready[1]);
        return -1;
    }
    if (child == 0) {
        close(ready[0]);
        if (write(ready[1], "R", 1) != 1) {
            _exit(1);
        }
        int accepted = syscall(SYS_accept4, listener, NULL, NULL, 0);
        char byte = 0;
        ssize_t received = accepted >= 0 ? recv(accepted, &byte, 1, 0) : -1;
        close_fd(accepted);
        _exit(received == 1 && byte == 'A' ? 0 : 1);
    }

    close(ready[1]);
    int result = read_ready(ready[0]);
    int client = -1;
    if (result == 0) {
        client = socket(AF_INET, SOCK_STREAM, 0);
        if (client < 0 ||
            connect(client, (struct sockaddr *)&address, sizeof(address)) < 0 ||
            send(client, "A", 1, MSG_NOSIGNAL) != 1) {
            perror("accept-wake: connect/send");
            result = -1;
        }
    }
    if (result == 0) {
        result = wait_child(child);
    } else {
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
    }
    close_fd(client);
    close(listener);
    close(ready[0]);
    return result;
}

static int test_nonblocking_send_backpressure(void)
{
    int client = -1;
    int server = -1;
    if (create_tcp_pair(&client, &server) < 0) {
        perror("send-eagain: create pair");
        return -1;
    }

    int send_buffer = 4096;
    int flags = fcntl(server, F_GETFL);
    if (flags < 0 || fcntl(server, F_SETFL, flags | O_NONBLOCK) < 0 ||
        setsockopt(server, SOL_SOCKET, SO_SNDBUF, &send_buffer,
                   sizeof(send_buffer)) < 0) {
        perror("send-eagain: setup");
        close(client);
        close(server);
        return -1;
    }

    static const char payload[16384];
    size_t total = 0;
    int saw_eagain = 0;
    for (int attempt = 0; attempt < 1024; ++attempt) {
        ssize_t sent = send(server, payload, sizeof(payload), MSG_DONTWAIT | MSG_NOSIGNAL);
        if (sent > 0) {
            total += (size_t)sent;
            continue;
        }
        if (sent < 0 && errno == EINTR) {
            --attempt;
            continue;
        }
        if (sent < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) {
            saw_eagain = 1;
            break;
        }
        perror("send-eagain: send");
        break;
    }

    close(client);
    close(server);
    if (!saw_eagain || total == 0) {
        fprintf(stderr, "send-eagain: total=%zu eagain=%d\n", total,
                saw_eagain);
        return -1;
    }
    return 0;
}

static int test_failed_connect_consumes_so_error(void)
{
    int reservation = -1;
    int client = -1;
    int result = -1;
    struct sockaddr_in address;

    reservation = socket(AF_INET, SOCK_STREAM, 0);
    memset(&address, 0, sizeof(address));
    address.sin_family = AF_INET;
    address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    address.sin_port = 0;
    if (reservation < 0 ||
        bind(reservation, (struct sockaddr *)&address, sizeof(address)) < 0) {
        perror("connect-error: reserve port");
        goto out;
    }
    socklen_t address_len = sizeof(address);
    if (getsockname(reservation, (struct sockaddr *)&address, &address_len) < 0) {
        perror("connect-error: getsockname");
        goto out;
    }

    client = socket(AF_INET, SOCK_STREAM | SOCK_NONBLOCK, 0);
    errno = 0;
    if (client < 0 ||
        syscall(SYS_connect, client, &address, sizeof(address)) != -1 ||
        errno != EINPROGRESS) {
        fprintf(stderr, "connect-error: expected EINPROGRESS, errno=%d\n", errno);
        goto out;
    }
    struct pollfd pollfd = {
        .fd = client,
        .events = POLLOUT | POLLERR,
    };
    if (raw_poll_wait(&pollfd, 1, EVENT_TIMEOUT_MS) != 1 ||
        !(pollfd.revents & (POLLOUT | POLLERR | POLLHUP))) {
        fprintf(stderr, "connect-error: missing completion event %#x\n",
                pollfd.revents);
        goto out;
    }

    int socket_error = 0;
    socklen_t error_len = sizeof(socket_error);
    if (syscall(SYS_getsockopt, client, SOL_SOCKET, SO_ERROR, &socket_error,
                &error_len) < 0 ||
        socket_error != ECONNREFUSED) {
        fprintf(stderr, "connect-error: first SO_ERROR=%d\n", socket_error);
        goto out;
    }
    socket_error = -1;
    error_len = sizeof(socket_error);
    if (syscall(SYS_getsockopt, client, SOL_SOCKET, SO_ERROR, &socket_error,
                &error_len) < 0 ||
        socket_error != 0) {
        fprintf(stderr, "connect-error: uncleared SO_ERROR=%d\n", socket_error);
        goto out;
    }
    result = 0;

out:
    close_fd(client);
    close_fd(reservation);
    return result;
}

enum signal_wait_kind {
    SIGNAL_WAIT_POLL,
    SIGNAL_WAIT_EPOLL,
};

static int run_signal_wait(enum signal_wait_kind kind)
{
    int client = -1;
    int server = -1;
    int ready[2] = {-1, -1};
    if (create_tcp_pair(&client, &server) < 0 || pipe(ready) < 0) {
        perror("wait-eintr: setup");
        return -1;
    }

    pid_t child = fork();
    if (child < 0) {
        perror("wait-eintr: fork");
        return -1;
    }
    if (child == 0) {
        close(client);
        close(ready[0]);
        got_usr1 = 0;
        struct sigaction action;
        memset(&action, 0, sizeof(action));
        action.sa_handler = on_usr1;
        sigemptyset(&action.sa_mask);
        if (sigaction(SIGUSR1, &action, NULL) < 0) {
            _exit(1);
        }

        int epfd = -1;
        struct epoll_event event;
        if (kind == SIGNAL_WAIT_EPOLL) {
            epfd = epoll_create1(EPOLL_CLOEXEC);
            struct epoll_event interest = {
                .events = EPOLLIN,
                .data.fd = server,
            };
            if (epfd < 0 ||
                epoll_ctl(epfd, EPOLL_CTL_ADD, server, &interest) < 0) {
                _exit(1);
            }
        }
        if (write(ready[1], "R", 1) != 1) {
            _exit(1);
        }

        errno = 0;
        long count;
        if (kind == SIGNAL_WAIT_POLL) {
            struct pollfd pollfd = {
                .fd = server,
                .events = POLLIN,
            };
            count = raw_poll_wait(&pollfd, 1, -1);
        } else {
            count = raw_epoll_wait(epfd, &event, -1);
        }
        int saved_errno = errno;
        close_fd(epfd);
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
    close(client);
    close(ready[0]);
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

static int test_signal_restarts_blocking_recv(void)
{
    int client = -1;
    int server = -1;
    int ready[2] = {-1, -1};
    if (create_tcp_pair(&client, &server) < 0 || pipe(ready) < 0) {
        perror("restart: setup");
        return -1;
    }

    pid_t child = fork();
    if (child < 0) {
        perror("restart: fork");
        return -1;
    }
    if (child == 0) {
        close(client);
        close(ready[0]);
        got_usr1 = 0;
        struct sigaction action;
        memset(&action, 0, sizeof(action));
        action.sa_handler = on_usr1;
        action.sa_flags = SA_RESTART;
        sigemptyset(&action.sa_mask);
        if (sigaction(SIGUSR1, &action, NULL) < 0 ||
            write(ready[1], "R", 1) != 1) {
            _exit(1);
        }
        char byte = 0;
        ssize_t count = syscall(SYS_recvfrom, server, &byte, 1, 0, NULL, NULL);
        _exit(count == 1 && byte == 'S' && got_usr1 ? 0 : 1);
    }

    close(server);
    close(ready[1]);
    int result = read_ready(ready[0]);
    if (result == 0 && kill(child, SIGUSR1) < 0) {
        result = -1;
    }
    if (result == 0) {
        usleep(WAIT_STEP_US);
        if (send(client, "S", 1, MSG_NOSIGNAL) != 1) {
            result = -1;
        }
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

struct concurrent_recv {
    int socket_fd;
    int ready_fd;
    ssize_t received;
    int saved_errno;
    char byte;
};

static void *run_concurrent_recv(void *opaque)
{
    struct concurrent_recv *context = opaque;
    if (write(context->ready_fd, "R", 1) != 1) {
        context->received = -1;
        context->saved_errno = EIO;
        return NULL;
    }
    errno = 0;
    context->received = recv(context->socket_fd, &context->byte, 1, 0);
    context->saved_errno = errno;
    return NULL;
}

static int test_concurrent_close_preserves_duplicate_waiter(void)
{
    int client = -1;
    int server = -1;
    int ready[2] = {-1, -1};
    if (create_tcp_pair(&client, &server) < 0 || pipe(ready) < 0) {
        perror("concurrent-close: setup");
        return -1;
    }
    int waiter_fd = dup(server);
    if (waiter_fd < 0) {
        perror("concurrent-close: dup");
        return -1;
    }

    struct concurrent_recv context = {
        .socket_fd = waiter_fd,
        .ready_fd = ready[1],
        .received = -1,
        .saved_errno = 0,
        .byte = 0,
    };
    pthread_t waiter;
    if (pthread_create(&waiter, NULL, run_concurrent_recv, &context) != 0 ||
        read_ready(ready[0]) < 0) {
        fprintf(stderr, "concurrent-close: failed to start waiter\n");
        return -1;
    }

    close(server);
    server = -1;
    if (send(client, "C", 1, MSG_NOSIGNAL) != 1 ||
        pthread_join(waiter, NULL) != 0) {
        perror("concurrent-close: send/join");
        return -1;
    }

    close(waiter_fd);
    close(ready[0]);
    close(ready[1]);
    close(client);
    return context.received == 1 && context.byte == 'C' ? 0 : -1;
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
    signal(SIGPIPE, SIG_IGN);
    if (test_blocking_data_and_epoll() < 0 ||
        test_nonblocking_connect_accept_and_poll() < 0 ||
        test_blocking_accept_wakeup() < 0 ||
        test_nonblocking_send_backpressure() < 0 ||
        test_failed_connect_consumes_so_error() < 0 ||
        run_signal_wait(SIGNAL_WAIT_POLL) < 0 ||
        run_signal_wait(SIGNAL_WAIT_EPOLL) < 0 ||
        test_signal_interrupts_blocking_recv() < 0 ||
        test_signal_restarts_blocking_recv() < 0 ||
        test_concurrent_close_preserves_duplicate_waiter() < 0 ||
        test_peer_close_wakes_epoll() < 0) {
        fprintf(stderr, "STARRY_GROUPED_TEST_FAILED: test-tcp-napi-runtime\n");
        return 1;
    }

    printf("STARRY_GROUPED_TEST_PASSED: test-tcp-napi-runtime\n");
    return 0;
}
