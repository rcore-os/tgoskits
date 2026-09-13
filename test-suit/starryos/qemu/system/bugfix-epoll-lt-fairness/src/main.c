#define _GNU_SOURCE

/*
 * epoll_wait(2) specifies round-robin delivery across successive calls when
 * more ready fds exist than maxevents can return. Keep one level-triggered fd
 * permanently readable ahead of a Unix listener and verify that maxevents=1
 * does not starve the listener.
 */

#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

#define DISTRACTOR_TOKEN UINT64_C(1)
#define LISTENER_TOKEN UINT64_C(2)
#define MAX_WAITS 8

static void timeout_handler(int signal_number)
{
    (void)signal_number;
    (void)write(
        STDERR_FILENO,
        "TIMEOUT: level-triggered epoll fairness\n",
        sizeof("TIMEOUT: level-triggered epoll fairness\n") - 1
    );
    (void)write(
        STDERR_FILENO,
        "STARRY_GROUPED_TEST_FAILED: bugfix-epoll-lt-fairness\n",
        sizeof("STARRY_GROUPED_TEST_FAILED: bugfix-epoll-lt-fairness\n") - 1
    );
    _exit(124);
}

static int fail(const char *operation)
{
    fprintf(
        stderr,
        "FAIL: %s errno=%d (%s)\n",
        operation,
        errno,
        strerror(errno)
    );
    puts("STARRY_GROUPED_TEST_FAILED: bugfix-epoll-lt-fairness");
    return EXIT_FAILURE;
}

static int add_interest(int epoll_fd, int fd, uint64_t token)
{
    struct epoll_event interest = {
        .events = EPOLLIN,
        .data.u64 = token,
    };
    return epoll_ctl(epoll_fd, EPOLL_CTL_ADD, fd, &interest);
}

int main(void)
{
    signal(SIGALRM, timeout_handler);
    alarm(15);

    char socket_path[sizeof(((struct sockaddr_un *)0)->sun_path)];
    int path_length = snprintf(
        socket_path,
        sizeof(socket_path),
        "/tmp/starry-epoll-fairness-%ld.sock",
        (long)getpid()
    );
    if (path_length < 0 || (size_t)path_length >= sizeof(socket_path)) {
        errno = ENAMETOOLONG;
        return fail("format Unix socket path");
    }
    unlink(socket_path);

    int listener = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC | SOCK_NONBLOCK, 0);
    if (listener < 0) {
        return fail("create Unix listener");
    }

    struct sockaddr_un address = {
        .sun_family = AF_UNIX,
    };
    memcpy(address.sun_path, socket_path, (size_t)path_length + 1);
    socklen_t address_length =
        (socklen_t)(offsetof(struct sockaddr_un, sun_path) + path_length + 1);
    if (bind(listener, (struct sockaddr *)&address, address_length) < 0 ||
        listen(listener, 8) < 0) {
        return fail("bind and listen");
    }

    int epoll_fd = epoll_create1(EPOLL_CLOEXEC);
    if (epoll_fd < 0) {
        return fail("create epoll");
    }

    int distractor[2];
    if (pipe2(distractor, O_CLOEXEC | O_NONBLOCK) < 0) {
        return fail("create distractor pipe");
    }
    if (write(distractor[1], "x", 1) != 1) {
        return fail("make distractor readable");
    }
    if (add_interest(epoll_fd, distractor[0], DISTRACTOR_TOKEN) < 0 ||
        add_interest(epoll_fd, listener, LISTENER_TOKEN) < 0) {
        return fail("register epoll interests");
    }

    int client = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (client < 0 ||
        connect(client, (struct sockaddr *)&address, address_length) < 0) {
        return fail("connect Unix client");
    }

    int listener_seen = 0;
    int distractor_seen = 0;
    for (int attempt = 0; attempt < MAX_WAITS; attempt++) {
        struct epoll_event event = {0};
        int count = epoll_wait(epoll_fd, &event, 1, 1000);
        if (count < 0 && errno == EINTR) {
            attempt--;
            continue;
        }
        if (count != 1) {
            if (count == 0) {
                errno = ETIMEDOUT;
            }
            return fail("wait for ready fd");
        }
        if (event.data.u64 == DISTRACTOR_TOKEN) {
            distractor_seen++;
        } else if (event.data.u64 == LISTENER_TOKEN) {
            listener_seen = 1;
            break;
        } else {
            errno = EPROTO;
            return fail("validate epoll token");
        }
    }

    if (!listener_seen) {
        errno = ETIMEDOUT;
        fprintf(
            stderr,
            "FAIL: listener starved behind level-triggered fd after %d waits; "
            "distractor returned %d times\n",
            MAX_WAITS,
            distractor_seen
        );
        puts("STARRY_GROUPED_TEST_FAILED: bugfix-epoll-lt-fairness");
        return EXIT_FAILURE;
    }

    int accepted = accept4(listener, NULL, NULL, SOCK_CLOEXEC | SOCK_NONBLOCK);
    if (accepted < 0) {
        return fail("accept queued Unix connection");
    }

    close(accepted);
    close(client);
    close(distractor[0]);
    close(distractor[1]);
    close(epoll_fd);
    close(listener);
    unlink(socket_path);

    printf(
        "PASS: listener returned after %d persistent distractor events\n",
        distractor_seen
    );
    puts("STARRY_EPOLL_LT_FAIRNESS_PASSED");
    puts("STARRY_GROUPED_TEST_PASSED: bugfix-epoll-lt-fairness");
    return EXIT_SUCCESS;
}
