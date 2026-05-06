#include <arpa/inet.h>
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

int main(void)
{
    int server = socket(AF_INET, SOCK_DGRAM, 0);
    int client = socket(AF_INET, SOCK_DGRAM, 0);
    if (server < 0 || client < 0) {
        printf("UDP_TEST_FAILED\n");
        return 1;
    }

    struct sockaddr_in bind_addr = {
        .sin_family = AF_INET,
        .sin_port = 0,
        .sin_addr = { .s_addr = htonl(INADDR_ANY) },
    };
    if (bind(server, (struct sockaddr *)&bind_addr, sizeof(bind_addr)) < 0) {
        printf("UDP_TEST_FAILED\n");
        return 1;
    }

    socklen_t bind_len = sizeof(bind_addr);
    if (getsockname(server, (struct sockaddr *)&bind_addr, &bind_len) < 0) {
        printf("UDP_TEST_FAILED\n");
        return 1;
    }

    struct sockaddr_in dst = {
        .sin_family = AF_INET,
        .sin_port = bind_addr.sin_port,
        .sin_addr = { .s_addr = htonl(INADDR_LOOPBACK) },
    };
    const char payload[] = "ping";
    if (sendto(client, payload, sizeof(payload), 0, (struct sockaddr *)&dst, sizeof(dst)) !=
        (ssize_t)sizeof(payload)) {
        printf("UDP_TEST_FAILED\n");
        return 1;
    }

    char buffer[16] = {0};
    struct sockaddr_in peer = {0};
    socklen_t peer_len = sizeof(peer);
    ssize_t n = recvfrom(server, buffer, sizeof(buffer), 0, (struct sockaddr *)&peer, &peer_len);
    if (n != (ssize_t)sizeof(payload) || memcmp(buffer, payload, sizeof(payload)) != 0) {
        printf("UDP_TEST_FAILED\n");
        return 1;
    }
    if (peer.sin_family != AF_INET || ntohl(peer.sin_addr.s_addr) != INADDR_LOOPBACK ||
        ntohs(peer.sin_port) == 0) {
        printf("UDP_TEST_FAILED\n");
        return 1;
    }

    close(client);
    close(server);
    printf("UDP_TEST_DONE\n");
    return 0;
}
