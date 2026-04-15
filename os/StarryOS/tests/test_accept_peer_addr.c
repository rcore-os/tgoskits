#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <errno.h>

int main() {
    int passed = 1;
    int listen_fd, client_fd, server_fd;
    struct sockaddr_in server_addr, client_addr, peer_addr;
    socklen_t client_len = sizeof(client_addr);
    socklen_t peer_len = sizeof(peer_addr);

    listen_fd = socket(AF_INET, SOCK_STREAM, 0);
    if (listen_fd < 0) {
        printf("SKIP: socket() failed: %s\n", strerror(errno));
        return 0;
    }

    memset(&server_addr, 0, sizeof(server_addr));
    server_addr.sin_family = AF_INET;
    server_addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    server_addr.sin_port = htons(0);

    if (bind(listen_fd, (struct sockaddr *)&server_addr, sizeof(server_addr)) < 0) {
        printf("SKIP: bind() failed: %s\n", strerror(errno));
        close(listen_fd);
        return 0;
    }

    socklen_t server_len = sizeof(server_addr);
    getsockname(listen_fd, (struct sockaddr *)&server_addr, &server_len);
    listen(listen_fd, 1);

    client_fd = socket(AF_INET, SOCK_STREAM, 0);
    if (client_fd < 0) {
        printf("SKIP: client socket() failed: %s\n", strerror(errno));
        close(listen_fd);
        return 0;
    }

    memset(&client_addr, 0, sizeof(client_addr));
    client_addr.sin_family = AF_INET;
    client_addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    client_addr.sin_port = htons(0);

    if (bind(client_fd, (struct sockaddr *)&client_addr, sizeof(client_addr)) < 0) {
        printf("SKIP: client bind() failed: %s\n", strerror(errno));
        close(listen_fd);
        close(client_fd);
        return 0;
    }

    socklen_t clen = sizeof(client_addr);
    getsockname(client_fd, (struct sockaddr *)&client_addr, &clen);

    if (connect(client_fd, (struct sockaddr *)&server_addr, sizeof(server_addr)) < 0) {
        printf("SKIP: connect() failed: %s\n", strerror(errno));
        close(listen_fd);
        close(client_fd);
        return 0;
    }

    server_fd = accept(listen_fd, (struct sockaddr *)&peer_addr, &peer_len);
    if (server_fd < 0) {
        printf("FAIL: accept() failed: %s\n", strerror(errno));
        passed = 0;
    } else {
        if (peer_addr.sin_port == client_addr.sin_port &&
            peer_addr.sin_addr.s_addr == client_addr.sin_addr.s_addr) {
            printf("PASS: accept() returned client (peer) address\n");
        } else if (peer_addr.sin_port == server_addr.sin_port) {
            printf("FAIL: accept() returned server (local) address instead of peer address\n");
            passed = 0;
        } else {
            printf("FAIL: accept() returned unexpected address\n");
            passed = 0;
        }
        close(server_fd);
    }

    close(client_fd);
    close(listen_fd);

    if (passed) {
        printf("\nAll tests PASSED\n");
        return 0;
    } else {
        printf("\nSome tests FAILED\n");
        return 1;
    }
}
