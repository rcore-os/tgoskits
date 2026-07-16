#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#define OUTPUT_FRAME_COUNT 120
#define OUTPUT_PAYLOAD_SIZE 192

static int write_all(int fd, const void *buffer, size_t length)
{
    const unsigned char *cursor = buffer;

    while (length != 0) {
        ssize_t written = write(fd, cursor, length);
        if (written > 0) {
            cursor += (size_t)written;
            length -= (size_t)written;
            continue;
        }
        if (written < 0 && errno == EINTR) {
            continue;
        }
        return -1;
    }
    return 0;
}

static int emit_frame(unsigned int sequence)
{
    unsigned char payload[OUTPUT_PAYLOAD_SIZE];
    char frame[320];

    for (size_t i = 0; i < sizeof(payload); ++i) {
        payload[i] = (unsigned char)('!' + ((sequence * 17U + (unsigned int)i * 29U) % 90U));
    }

    int prefix = snprintf(frame, sizeof(frame), "STARRY_TTY_OUTPUT_BURST_FRAME:%03u:",
                          sequence);
    if (prefix < 0 || (size_t)prefix + sizeof(payload) + 1 > sizeof(frame)) {
        return -1;
    }
    memcpy(frame + prefix, payload, sizeof(payload));
    frame[(size_t)prefix + sizeof(payload)] = '\n';
    return write_all(STDOUT_FILENO, frame, (size_t)prefix + sizeof(payload) + 1);
}

int main(void)
{
    for (unsigned int sequence = 1; sequence <= OUTPUT_FRAME_COUNT; ++sequence) {
        if (emit_frame(sequence) != 0) {
            static const char failed[] = "STARRY_TTY_OUTPUT_BURST_FAILED:write\n";
            (void)write_all(STDERR_FILENO, failed, sizeof(failed) - 1);
            return 1;
        }
    }

    static const char passed[] = "STARRY_TTY_OUTPUT_BURST_PASSED\n";
    if (write_all(STDOUT_FILENO, passed, sizeof(passed) - 1) != 0) {
        return 1;
    }
    return 0;
}
