/*
 * bug-tcp-send-no-epoll-notify: writing to a loopback TCP socket must
 * trigger EPOLLIN on the peer socket's epoll registration.
 *
 * Root cause: TcpSocket::send() called poll_interfaces() before writing
 * to the socket buffer but not after, so the loopback packet was never
 * transmitted and the peer socket's epoll waker never fired.
 *
 * Fix: call poll_interfaces() after writing in TcpSocket::send().
 */
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/socket.h>
#include <unistd.h>

int main(void) {
    /* --- create a connected loopback TCP pair --- */
    int listen_fd = socket(AF_INET, SOCK_STREAM, 0);
    if (listen_fd < 0) {
        fprintf(stderr, "socket(listen): %s\n", strerror(errno));
        return 1;
    }
    int opt = 1;
    if (setsockopt(listen_fd, SOL_SOCKET, SO_REUSEADDR, &opt, sizeof(opt)) < 0) {
        fprintf(stderr, "setsockopt: %s\n", strerror(errno));
        return 1;
    }

    struct sockaddr_in addr = {
        .sin_family = AF_INET,
        .sin_addr.s_addr = htonl(INADDR_LOOPBACK),
        .sin_port = 0,
    };
    if (bind(listen_fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        fprintf(stderr, "bind: %s\n", strerror(errno));
        return 1;
    }
    if (listen(listen_fd, 1) < 0) {
        fprintf(stderr, "listen: %s\n", strerror(errno));
        return 1;
    }

    socklen_t len = sizeof(addr);
    getsockname(listen_fd, (struct sockaddr *)&addr, &len);

    int client_fd = socket(AF_INET, SOCK_STREAM, 0);
    if (client_fd < 0) {
        fprintf(stderr, "socket(client): %s\n", strerror(errno));
        return 1;
    }
    if (connect(client_fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        fprintf(stderr, "connect: %s\n", strerror(errno));
        return 1;
    }

    int server_fd = accept(listen_fd, NULL, NULL);
    if (server_fd < 0) {
        fprintf(stderr, "accept: %s\n", strerror(errno));
        return 1;
    }
    close(listen_fd);

    /* set both non-blocking */
    fcntl(client_fd, F_SETFL, O_NONBLOCK);
    fcntl(server_fd, F_SETFL, O_NONBLOCK);

    /* --- register client socket with epoll --- */
    int epfd = epoll_create1(EPOLL_CLOEXEC);
    if (epfd < 0) {
        fprintf(stderr, "epoll_create1: %s\n", strerror(errno));
        return 1;
    }
    struct epoll_event ev = {
        .events = EPOLLIN | EPOLLET,
        .data.fd = client_fd,
    };
    if (epoll_ctl(epfd, EPOLL_CTL_ADD, client_fd, &ev) < 0) {
        fprintf(stderr, "epoll_ctl: %s\n", strerror(errno));
        return 1;
    }

    /* --- server writes data --- */
    if (write(server_fd, "hello", 5) < 0) {
        fprintf(stderr, "write: %s\n", strerror(errno));
        return 1;
    }

    /* --- epoll_wait must return EPOLLIN on client_fd ---
     * Generous 1s timeout to avoid flakiness on slow QEMU boot. */
    struct epoll_event events[4];
    int n = epoll_wait(epfd, events, 4, 1000);

    close(epfd);
    close(client_fd);
    close(server_fd);

    if (n == 1 && events[0].data.fd == client_fd) {
        printf("STARRY_GROUPED_TEST_PASSED: bug-tcp-send-no-epoll-notify\n");
        return 0;
    }

    fprintf(stderr, "STARRY_GROUPED_TEST_FAILED: bug-tcp-send-no-epoll-notify "
                    "(epoll_wait returned %d events, expected 1)\n", n);
    return 1;
}
