// Copyright 2026 The TGOSKits Authors
//
// SPDX-License-Identifier: Apache-2.0

#define _POSIX_C_SOURCE 200809L

#include "aicp_client.h"
#include "aicp_posix_stream.h"

#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static void sleep_ms(long milliseconds) {
    const struct timespec duration = {
        .tv_sec = milliseconds / 1000,
        .tv_nsec = (milliseconds % 1000) * 1000000L,
    };

    (void)nanosleep(&duration, NULL);
}

static int reserve_local_port(void) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) {
        return -1;
    }
    struct sockaddr_in address = {
        .sin_family = AF_INET,
        .sin_port = 0,
    };
    address.sin_addr.s_addr = htonl(0x7f000001u);
    socklen_t length = sizeof(address);
    if (bind(fd, (const struct sockaddr *)&address, sizeof(address)) != 0 ||
        getsockname(fd, (struct sockaddr *)&address, &length) != 0) {
        close(fd);
        return -1;
    }
    const int port = ntohs(address.sin_port);
    close(fd);
    return port;
}

static int connect_to_server(int port) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) {
        return -1;
    }
    struct sockaddr_in address = {
        .sin_family = AF_INET,
        .sin_port = htons((uint16_t)port),
    };
    address.sin_addr.s_addr = htonl(0x7f000001u);
    if (connect(fd, (const struct sockaddr *)&address, sizeof(address)) != 0) {
        close(fd);
        return -1;
    }
    return fd;
}

static int wait_for_server(int port) {
    for (unsigned attempt = 0; attempt != 100; ++attempt) {
        const int fd = connect_to_server(port);
        if (fd >= 0) {
            close(fd);
            return 0;
        }
        sleep_ms(10);
    }
    return -1;
}

static int expect_bad_type_error(int fd, uint32_t sequence) {
    struct aicp_header request = aicp_make_header(
        0x7f, 0, 0, sequence, 0, AICP_OK);
    if (aicp_posix_send_frame(fd, request, NULL) != 0) {
        return -1;
    }

    struct aicp_header response;
    uint8_t payload[AICP_MAX_PAYLOAD];
    if (aicp_posix_recv_frame(fd, &response, payload, sizeof(payload)) != 0) {
        return -1;
    }
    return response.msg_type == AICP_MSG_ERROR && response.seq == sequence &&
                   response.error_code == AICP_ERR_BAD_TYPE
               ? 0
               : -1;
}

static int run_smoke(int port) {
    const int slow_client = connect_to_server(port);
    if (slow_client < 0 || write(slow_client, "A", 1) != 1) {
        return -1;
    }
    sleep_ms(500);
    if (write(slow_client, "I", 1) != 1) {
        close(slow_client);
        return -1;
    }
    sleep_ms(700);

    const int active_client = connect_to_server(port);
    if (active_client < 0) {
        close(slow_client);
        return -1;
    }

    struct aicp_posix_stream stream;
    aicp_posix_stream_init(&stream, active_client);
    uint32_t next_sequence = 1;
    struct aicp_status_payload status;
    int result = aicp_client_session_handshake(
        &stream.stream,
        &next_sequence,
        "{\"role\":\"smoke\"}",
        sizeof("{\"role\":\"smoke\"}"),
        &status,
        NULL);
    if (result == 0 && status.applied_seq != 0) {
        result = -1;
    }
    if (result == 0) {
        result = expect_bad_type_error(active_client, next_sequence++);
    }

    close(active_client);
    close(slow_client);
    return result;
}

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: %s <reference-server>\n", argv[0]);
        return 2;
    }

    const int port = reserve_local_port();
    if (port < 0) {
        perror("reserve_local_port");
        return 1;
    }
    char port_text[16];
    (void)snprintf(port_text, sizeof(port_text), "%d", port);

    const pid_t server = fork();
    if (server < 0) {
        perror("fork");
        return 1;
    }
    if (server == 0) {
        execl(argv[1], argv[1], port_text, (char *)NULL);
        _exit(127);
    }

    int result = wait_for_server(port);
    if (result == 0) {
        result = run_smoke(port);
    }
    (void)kill(server, SIGTERM);
    if (waitpid(server, NULL, 0) < 0) {
        perror("waitpid");
        result = -1;
    }
    if (result != 0) {
        fprintf(stderr, "AICP reference server smoke failed\n");
        return 1;
    }

    printf("AICP_REFERENCE_SERVER_SMOKE_PASSED\n");
    return 0;
}
