#include <errno.h>
#include <fcntl.h>
#include <mqueue.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#define QUEUE_NAME "/mqueue_send_validation"

int main(void)
{
    const char message[] = "12345678";
    struct mq_attr attr = {
        .mq_maxmsg = 1,
        .mq_msgsize = sizeof(message) - 1,
    };
    mqd_t queue = (mqd_t)-1;
    int result = 1;

    queue = mq_open(QUEUE_NAME, O_CREAT | O_EXCL | O_RDWR, 0600, &attr);
    if (queue == (mqd_t)-1) {
        perror("mq_open");
        goto cleanup;
    }

    /*
     * Linux validates msg_len against mq_msgsize before dereferencing the
     * message pointer. This invalid pointer must therefore yield EMSGSIZE,
     * rather than EFAULT from a premature user-memory copy.
     */
    errno = 0;
    if (mq_send(queue, (const char *)(uintptr_t)1, sizeof(message), 0) != -1
        || errno != EMSGSIZE) {
        fprintf(stderr, "oversize mq_send returned errno=%d, expected EMSGSIZE\n", errno);
        goto cleanup;
    }

    if (mq_send(queue, message, sizeof(message) - 1, 0) != 0) {
        perror("mq_send at mq_msgsize");
        goto cleanup;
    }

    char received[sizeof(message)] = { 0 };
    if (mq_receive(queue, received, sizeof(received) - 1, NULL) != sizeof(message) - 1
        || memcmp(received, message, sizeof(message) - 1) != 0) {
        perror("mq_receive");
        goto cleanup;
    }

    result = 0;

cleanup:
    if (queue != (mqd_t)-1) {
        mq_close(queue);
    }
    mq_unlink(QUEUE_NAME);
    return result;
}
