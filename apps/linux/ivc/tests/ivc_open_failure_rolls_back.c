#include <ivc/ioctl_args.h>
#include <ivc/ulib.h>

#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static unsigned long expected_create_request;
static unsigned long expected_rollback_request;
static int create_requests;
static int rollback_requests;
static int open_requests;

static void reset_mocks(unsigned long create_request, unsigned long rollback_request)
{
    expected_create_request = create_request;
    expected_rollback_request = rollback_request;
    create_requests = 0;
    rollback_requests = 0;
    open_requests = 0;
}

int __wrap_ioctl(int fd, unsigned long request, ...)
{
    va_list args;
    void *arg;

    (void)fd;

    va_start(args, request);
    arg = va_arg(args, void *);
    va_end(args);

    if (request == expected_create_request) {
        create_requests++;
        if (request == IVC_PUBLISH_CHANNEL) {
            ivc_publish_arg_p publish_arg = arg;
            strncpy(publish_arg->device_name, "/dev/mock-publisher", sizeof(publish_arg->device_name) - 1);
        } else {
            ivc_subscribe_arg_p subscribe_arg = arg;
            strncpy(subscribe_arg->device_name, "/dev/mock-subscriber", sizeof(subscribe_arg->device_name) - 1);
        }
        return 0;
    }

    if (request == expected_rollback_request) {
        rollback_requests++;
        return 0;
    }

    fprintf(stderr, "unexpected ioctl request: %lu\n", request);
    return -1;
}

int __wrap_open(const char *path, int flags, ...)
{
    (void)path;
    (void)flags;

    open_requests++;
    errno = ENOENT;
    return -1;
}

static int expect_rollback(const char *operation)
{
    if (create_requests != 1) {
        fprintf(stderr, "%s: expected one create ioctl, got %d\n", operation, create_requests);
        return 1;
    }
    if (open_requests != 1) {
        fprintf(stderr, "%s: expected one open call, got %d\n", operation, open_requests);
        return 1;
    }
    if (rollback_requests != 1) {
        fprintf(stderr, "%s: expected one rollback ioctl, got %d\n", operation, rollback_requests);
        return 1;
    }
    return 0;
}

static int publish_open_failure_rolls_back(void)
{
    ivc_manager_t manager = {
        .fd = 7,
    };

    reset_mocks(IVC_PUBLISH_CHANNEL, IVC_UNPUBLISH_CHANNEL);

    if (ivc_publish(&manager, 0x100, 4096) != NULL) {
        fprintf(stderr, "publish: expected open failure\n");
        return 1;
    }

    return expect_rollback("publish");
}

static int subscribe_open_failure_rolls_back(void)
{
    ivc_manager_t manager = {
        .fd = 7,
    };

    reset_mocks(IVC_SUBSCRIBE_CHANNEL, IVC_UNSUBSCRIBE_CHANNEL);

    if (ivc_subscribe(&manager, 1, 0x100) != NULL) {
        fprintf(stderr, "subscribe: expected open failure\n");
        return 1;
    }

    return expect_rollback("subscribe");
}

int main(void)
{
    if (publish_open_failure_rolls_back() != 0) {
        return 1;
    }
    if (subscribe_open_failure_rolls_back() != 0) {
        return 1;
    }

    return 0;
}
