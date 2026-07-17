#pragma once

#include "./ivc_dev.h"

#ifndef __KERNEL__
# include <sys/ioctl.h>
# include <stdint.h>
#endif

typedef struct ivc_publish_arg {
    uint64_t    channel_key;                            // Unique key for the channel
    uint64_t    channel_size;                           // Size of the channel in bytes
    char        device_name[MAX_IVC_DEV_NAME_LENGTH];   // Returns the device name for the channel
} ivc_publish_arg_t, *ivc_publish_arg_p;

typedef struct ivc_subscribe_arg {
    uint64_t    target_publisher_id;                    // ID of the publisher to subscribe to (VM ID is used as for now)
    uint64_t    channel_key;                            // Key of the channel to subscribe to
    char        device_name[MAX_IVC_DEV_NAME_LENGTH];   // Returns the device name for the channel
} ivc_subscribe_arg_t, *ivc_subscribe_arg_p;

#define IVC_PUBLISH_CHANNEL _IOW(0, 0, ivc_publish_arg_t)
#define IVC_UNPUBLISH_CHANNEL _IOW(0, 1, ivc_publish_arg_t)
#define IVC_SUBSCRIBE_CHANNEL _IOW(0, 2, ivc_subscribe_arg_t)
#define IVC_UNSUBSCRIBE_CHANNEL _IOW(0, 3, ivc_subscribe_arg_t)
