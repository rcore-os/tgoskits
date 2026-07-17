#include <ivc/ulib.h>

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static void print_usage(const char *program)
{
    fprintf(stderr,
            "Usage: %s <channel_key> [message_count] [message] [channel_size] [interval_ms]\n",
            program);
}

int main(int argc, char *argv[])
{
    unsigned long message_count = 5;
    const char *message = "hello from linux publisher";
    unsigned long channel_size = 4096;
    unsigned long interval_ms = 100;
    int ret = 0;

    if (argc < 2 || argc > 6) {
        print_usage(argv[0]);
        return 1;
    }

    uint64_t channel_key = strtoull(argv[1], NULL, 0);
    if (argc >= 3) {
        message_count = strtoul(argv[2], NULL, 0);
    }
    if (argc >= 4) {
        message = argv[3];
    }
    if (argc >= 5) {
        channel_size = strtoul(argv[4], NULL, 0);
    }
    if (argc >= 6) {
        interval_ms = strtoul(argv[5], NULL, 0);
    }

    ivc_manager_p manager = ivc_open_manager();
    if (!manager) {
        fprintf(stderr, "Failed to open IVC manager\n");
        return 1;
    }

    ivc_publisher_p publisher = ivc_publish(manager, channel_key, channel_size);
    if (!publisher) {
        fprintf(stderr, "Failed to publish channel\n");
        ret = 2;
        goto close_manager;
    }

    for (unsigned long sent = 0; sent < message_count; sent++) {
        int bytes_written = ivc_write_all(publisher, message, strlen(message));
        if (bytes_written < 0) {
            fprintf(stderr, "Failed to write IVC message %lu/%lu\n",
                    sent + 1, message_count);
            ret = 3;
            break;
        }

        printf("linux ivc publish %lu/%lu: %s\n", sent + 1, message_count,
               message);
        if (interval_ms > 0 && sent + 1 < message_count) {
            usleep(interval_ms * 1000);
        }
    }

    if (ret == 0) {
        printf("linux ivc publisher pass\n");
    }

    if (ivc_unpublish(publisher) < 0) {
        fprintf(stderr, "Failed to unpublish channel\n");
        ret = 4;
    }

close_manager:
    if (ivc_close_manager(manager) < 0) {
        fprintf(stderr, "Failed to close IVC manager\n");
        ret = 5;
    }
    printf("IVC publisher example finished.\n");
    return ret;
}
