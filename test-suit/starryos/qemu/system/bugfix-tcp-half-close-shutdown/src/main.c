#define _GNU_SOURCE

#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

static int failures;

static void check(int condition, const char *message)
{
    if (condition) {
        printf("PASS: %s\n", message);
    } else {
        printf("FAIL: %s (errno=%d: %s)\n", message, errno, strerror(errno));
        ++failures;
    }
}

int main(void)
{
    static const char reply[] = "half-close reply";
    struct sockaddr_in address = {
        .sin_family = AF_INET,
        .sin_addr.s_addr = htonl(INADDR_LOOPBACK),
    };
    socklen_t address_length = sizeof(address);
    int listener = socket(AF_INET, SOCK_STREAM, 0);
    check(listener >= 0, "create TCP listener");
    if (listener < 0) {
        return 1;
    }

    int setup_ok = bind(listener, (struct sockaddr *)&address, sizeof(address)) == 0;
    check(setup_ok, "bind TCP listener to loopback");
    setup_ok = setup_ok
        && getsockname(listener, (struct sockaddr *)&address, &address_length) == 0;
    check(setup_ok, "obtain the ephemeral listener port");
    setup_ok = setup_ok && listen(listener, 1) == 0;
    check(setup_ok, "listen for the half-close client");
    if (!setup_ok) {
        close(listener);
        return 1;
    }

    int client = socket(AF_INET, SOCK_STREAM, 0);
    check(client >= 0, "create TCP client");
    if (client < 0) {
        close(listener);
        return 1;
    }

    int connected = connect(client, (struct sockaddr *)&address, sizeof(address)) == 0;
    check(connected, "connect TCP client");
    if (!connected) {
        close(client);
        close(listener);
        return 1;
    }
    int peer = accept(listener, NULL, NULL);
    check(peer >= 0, "accept TCP client");
    close(listener);
    if (peer < 0) {
        close(client);
        return 1;
    }

    check(shutdown(client, SHUT_WR) == 0, "shutdown the client write side");
    check(send(peer, reply, sizeof(reply) - 1, 0) == (ssize_t)(sizeof(reply) - 1),
          "send a reply after the client half-close");

    struct pollfd client_poll = {
        .fd = client,
        .events = POLLIN,
    };
    int ready = poll(&client_poll, 1, 3000);
    check(ready == 1 && (client_poll.revents & POLLIN) != 0,
          "poll wakes for the reply after shutdown(SHUT_WR)");

    if (ready == 1 && (client_poll.revents & POLLIN) != 0) {
        char received[sizeof(reply)] = { 0 };
        ssize_t length = recv(client, received, sizeof(received) - 1, 0);
        check(length == (ssize_t)(sizeof(reply) - 1)
                  && memcmp(received, reply, sizeof(reply) - 1) == 0,
              "receive the reply after shutdown(SHUT_WR)");
    }

    close(peer);
    close(client);
    if (failures == 0) {
        printf("TEST PASSED\n");
        return 0;
    }
    printf("TEST FAILED: %d checks\n", failures);
    return 1;
}
