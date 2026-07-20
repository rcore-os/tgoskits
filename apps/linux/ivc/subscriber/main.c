#include <ivc/ulib.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

char message[1024];
int main(int argc, char *argv[]) {
    unsigned long target_count = 5;
    unsigned long received = 0;
    unsigned long empty_polls = 0;

    if (argc != 3 && argc != 4) {
        fprintf(stderr, "Usage: %s <target_publisher_id> <channel_key> [message_count]\n", argv[0]);
        return 1;
    }
    uint64_t target_publisher_id = strtoull(argv[1], NULL, 0);
    uint64_t channel_key = strtoull(argv[2], NULL, 0);
    if (argc == 4) {
        target_count = strtoul(argv[3], NULL, 0);
    }

    int ret = 0;

    ivc_manager_p manager = ivc_open_manager();
    if (!manager) {
        fprintf(stderr, "Failed to open IVC manager\n");
        return 1;
    }

    ivc_subscriber_p subscriber = ivc_subscribe(manager, target_publisher_id, channel_key);
    if (!subscriber) {
        fprintf(stderr, "Failed to subscribe to channel\n");
        ret = 2;
        goto close_manager;
    }

    while (received < target_count) {
        int bytes_read = ivc_read(subscriber, message, sizeof(message) - 1);
        if (bytes_read < 0) {
            fprintf(stderr, "Failed to read from subscriber\n");
            ret = 2;
            break;
        } else if (bytes_read == 0) {
            if (++empty_polls > 200000) {
                fprintf(stderr, "Timed out waiting for IVC messages\n");
                ret = 5;
                break;
            }
            usleep(10000);
        } else {
            message[bytes_read] = '\0'; // Null-terminate the string
            received++;
            empty_polls = 0;
            printf("linux ivc recv %lu/%lu: %s\n", received, target_count, message);
            if (write(subscriber->fd, "ack from linux subscriber", 25) < 0) {
                perror("Failed to send IVC ack");
                ret = 6;
                break;
            }
        }
    }
    if (ret == 0) {
        printf("linux ivc demo pass\n");
    }

    if (ivc_unsubscribe(subscriber) < 0) {
        fprintf(stderr, "Failed to unsubscribe from channel\n");
        ret = 3;
    }
close_manager:
    if (ivc_close_manager(manager) < 0) {
        fprintf(stderr, "Failed to close IVC manager\n");
        ret = 4;
    }
    printf("IVC subscriber example finished.\n");
    return ret;
}
