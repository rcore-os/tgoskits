#include <ivc/ulib.h>

#include <ivc/ivc_dev.h>
#include <ivc/ioctl_args.h>

#include <stdlib.h>
#include <string.h>
#include <fcntl.h>
#include <unistd.h>


int ivc_manager_is_valid(ivc_manager_p manager) {
    // Check if the manager is valid
    if (!manager || (manager->fd < 0)) {
        return 0;
    }
    return 1;
}

ivc_manager_p ivc_open_manager(void) {
    // Allocate memory for the IVC manager
    ivc_manager_p manager = malloc(sizeof(ivc_manager_t));
    if (!manager) {
        perror("Failed to allocate memory for IVC manager");
        return NULL;
    }
    // Open the IVC device
    manager->fd = open(IVC_DEV_PATH, O_RDWR);
    if (manager->fd < 0) {
        perror("Failed to open IVC device");
        free(manager);
        return NULL;
    }
    return manager;
}

int ivc_close_manager(ivc_manager_p manager) {
    // Check if the manager is valid
    if (!ivc_manager_is_valid(manager)) {
        fprintf(stderr, "Invalid IVC manager for close\n");
        return -1;
    }
    // Close the IVC device
    if (close(manager->fd) < 0) {
        perror("Failed to close IVC device");
        return -1;
    }
    // Free the manager memory
    free(manager);
    return 0;
}

ivc_subscriber_p ivc_subscribe(ivc_manager_p manager, uint64_t publisher_id, uint64_t channel_key) {
    // Check if the manager is valid
    if (!ivc_manager_is_valid(manager)) {
        fprintf(stderr, "Invalid IVC manager for subscribe\n");
        return NULL;
    }

    // Allocate memory for the subscriber
    ivc_subscriber_p subscriber = malloc(sizeof(ivc_subscriber_t));
    if (!subscriber) {
        perror("Failed to allocate memory for subscriber");
        return NULL;
    }

    // Set up the subscriber structure
    subscriber->manager = manager;
    subscriber->subscribe_arg.target_publisher_id = publisher_id;
    subscriber->subscribe_arg.channel_key = channel_key;
    memset(subscriber->subscribe_arg.device_name, 0, sizeof(subscriber->subscribe_arg.device_name));
    subscriber->read = 0;

    // Perform the subscription operation
    if (ioctl(manager->fd, IVC_SUBSCRIBE_CHANNEL, &subscriber->subscribe_arg) < 0) {
        perror("Failed to subscribe to channel");
        free(subscriber);
        return NULL;
    }

    // Open the subscriber device
    subscriber->fd = open(subscriber->subscribe_arg.device_name, O_RDONLY);
    if (subscriber->fd < 0) {
        perror("Failed to open subscriber device");
        free(subscriber);
        return NULL;
    }

    return subscriber;
}

int ivc_read(ivc_subscriber_p subscriber, void *buf, size_t count) {
    if (count == 0) {
        return 0; // Nothing to read
    }

    if (!subscriber || !buf) {
        fprintf(stderr, "Invalid arguments for ivc_read\n");
        return -1;
    }

    int bytes_read = read(subscriber->fd, buf, count);
    if (bytes_read < 0) {
        perror("Failed to read from subscriber device");
    } else {
        subscriber->read += bytes_read;
    }
    return bytes_read;
}

int ivc_unsubscribe(ivc_subscriber_p subscriber) {
    if (!subscriber) {
        fprintf(stderr, "Invalid subscriber for ivc_unsubscribe\n");
        return -1;
    }

    // Close the subscriber device
    if (close(subscriber->fd) < 0) {
        perror("Failed to close subscriber device");
        return -1;
    }

    // Perform the unsubscribe operation
    if (ioctl(subscriber->manager->fd, IVC_UNSUBSCRIBE_CHANNEL, &subscriber->subscribe_arg) < 0) {
        perror("Failed to unsubscribe from channel");
        return -1;
    }

    // Free the subscriber memory
    free(subscriber);
    return 0;
}

ivc_publisher_p ivc_publish(ivc_manager_p manager, uint64_t channel_key, uint64_t channel_size) {
    // Check if the manager is valid
    if (!ivc_manager_is_valid(manager)) {
        fprintf(stderr, "Invalid IVC manager for publish\n");
        return NULL;
    }

    // Allocate memory for the publisher
    ivc_publisher_p publisher = malloc(sizeof(ivc_publisher_t));
    if (!publisher) {
        perror("Failed to allocate memory for publisher");
        return NULL;
    }

    // Set up the publisher structure
    publisher->manager = manager;
    publisher->publish_arg.channel_key = channel_key;
    publisher->publish_arg.channel_size = channel_size;
    memset(publisher->publish_arg.device_name, 0, sizeof(publisher->publish_arg.device_name));
    publisher->write = 0;

    // Perform the publish operation
    if (ioctl(manager->fd, IVC_PUBLISH_CHANNEL, &publisher->publish_arg) < 0) {
        perror("Failed to publish channel");
        free(publisher);
        return NULL;
    }

    // Open the publisher device
    publisher->fd = open(publisher->publish_arg.device_name, O_WRONLY);
    if (publisher->fd < 0) {
        perror("Failed to open publisher device");
        free(publisher);
        return NULL;
    }
    
    return publisher;
}

int ivc_write(ivc_publisher_p publisher, const void *buf, size_t count) {
    if (count == 0) {
        return 0; // Nothing to write
    }

    if (!publisher || !buf) {
        fprintf(stderr, "Invalid arguments for ivc_write\n");
        return -1;
    }

    int bytes_written = write(publisher->fd, buf, count);
    if (bytes_written < 0) {
        perror("Failed to write to publisher device");
    } else {
        publisher->write += bytes_written;
    }
    return bytes_written;
}

int ivc_write_all(ivc_publisher_p publisher, const void *buf, size_t count) {
    if (count == 0) {
        return 0; // Nothing to write
    }

    size_t total_written = 0;
    while (total_written < count) {
        int bytes_written = ivc_write(publisher, (const char *)buf + total_written, count - total_written);
        if (bytes_written < 0) {
            perror("Failed to write to publisher device");
            return -1;
        }
        total_written += bytes_written;
    }
    return total_written;
}

int ivc_unpublish(ivc_publisher_p publisher) {
    if (!publisher) {
        fprintf(stderr, "Invalid publisher for ivc_unpublish\n");
        return -1;
    }

    // Close the publisher device
    if (close(publisher->fd) < 0) {
        perror("Failed to close publisher device");
        return -1;
    }

    // Perform the unpublish operation
    if (ioctl(publisher->manager->fd, IVC_UNPUBLISH_CHANNEL, &publisher->publish_arg) < 0) {
        perror("Failed to unpublish channel");
        return -1;
    }

    // Free the publisher memory
    free(publisher);
    return 0;
}
