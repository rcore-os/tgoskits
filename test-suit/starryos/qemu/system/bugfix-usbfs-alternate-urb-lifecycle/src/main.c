#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <libusb.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

#define USB_AUDIO_CLASS 0x01
#define USB_AUDIO_SUBCLASS_STREAMING 0x02
/* QEMU usb-audio is full-speed, so 2048 ISO packets span more than usbfs's
 * one-second cancellation deadline and remain in flight at SETINTERFACE. */
#define QUIESCED_URB_PACKETS 2048
#define BARRIER_URB_PACKETS 8

#define USBFS_URB_TYPE_ISO 0
#define USBFS_URB_ISO_ASAP 0x02

struct usbfs_setinterface {
    unsigned int interface;
    unsigned int altsetting;
};

struct usbfs_iso_packet_desc {
    unsigned int length;
    unsigned int actual_length;
    unsigned int status;
};

struct usbfs_urb {
    unsigned char type;
    unsigned char endpoint;
    int status;
    unsigned int flags;
    void *buffer;
    int buffer_length;
    int actual_length;
    int start_frame;
    int number_of_packets;
    int error_count;
    unsigned int signr;
    void *usercontext;
    struct usbfs_iso_packet_desc iso_frame_desc[];
};

#define USBFS_SETINTERFACE _IOR('U', 4, struct usbfs_setinterface)
#define USBFS_SUBMITURB _IOR('U', 10, struct usbfs_urb)
#define USBFS_REAPURB _IOW('U', 12, void *)
#define USBFS_REAPURBNDELAY _IOW('U', 13, void *)
#define USBFS_CLAIMINTERFACE _IOR('U', 15, unsigned int)
#define USBFS_RELEASEINTERFACE _IOR('U', 16, unsigned int)

typedef struct audio_endpoint {
    uint8_t bus_number;
    uint8_t device_address;
    uint8_t configuration;
    uint8_t interface_number;
    uint8_t alternate;
    uint8_t endpoint;
    int packet_size;
} audio_endpoint_t;

typedef struct iso_urb {
    struct usbfs_urb *urb;
    unsigned char *buffer;
} iso_urb_t;

static int failf(const char *format, ...) {
    va_list args;
    va_start(args, format);
    fputs("usbfs alternate URB lifecycle failed: ", stdout);
    vprintf(format, args);
    fputc('\n', stdout);
    va_end(args);
    return 1;
}

static bool is_audio_iso_out(
    libusb_device *device,
    const struct libusb_interface_descriptor *interface,
    uint8_t *endpoint,
    int *packet_size
) {
    if (interface->bInterfaceClass != USB_AUDIO_CLASS ||
        interface->bInterfaceSubClass != USB_AUDIO_SUBCLASS_STREAMING ||
        interface->bAlternateSetting == 0) {
        return false;
    }

    for (int index = 0; index < interface->bNumEndpoints; index++) {
        const struct libusb_endpoint_descriptor *descriptor = &interface->endpoint[index];
        if ((descriptor->bmAttributes & LIBUSB_TRANSFER_TYPE_MASK) !=
                LIBUSB_TRANSFER_TYPE_ISOCHRONOUS ||
            (descriptor->bEndpointAddress & LIBUSB_ENDPOINT_DIR_MASK) != LIBUSB_ENDPOINT_OUT) {
            continue;
        }
        int size = libusb_get_max_iso_packet_size(device, descriptor->bEndpointAddress);
        if (size > 0) {
            *endpoint = descriptor->bEndpointAddress;
            *packet_size = size;
            return true;
        }
    }
    return false;
}

static int find_audio_endpoint(libusb_context *context, audio_endpoint_t *candidate) {
    libusb_device **devices = NULL;
    ssize_t count = libusb_get_device_list(context, &devices);
    if (count < 0) {
        return failf("libusb_get_device_list: %s", libusb_error_name((int)count));
    }

    memset(candidate, 0, sizeof(*candidate));
    for (ssize_t device_index = 0; device_index < count && candidate->packet_size == 0;
         device_index++) {
        libusb_device *device = devices[device_index];
        struct libusb_device_descriptor device_descriptor;
        if (libusb_get_device_descriptor(device, &device_descriptor) != 0) {
            continue;
        }
        for (uint8_t config_index = 0;
             config_index < device_descriptor.bNumConfigurations && candidate->packet_size == 0;
             config_index++) {
            struct libusb_config_descriptor *configuration = NULL;
            if (libusb_get_config_descriptor(device, config_index, &configuration) != 0 ||
                configuration == NULL) {
                continue;
            }
            for (int interface_index = 0;
                 interface_index < configuration->bNumInterfaces && candidate->packet_size == 0;
                 interface_index++) {
                const struct libusb_interface *interface =
                    &configuration->interface[interface_index];
                for (int alternate_index = 0; alternate_index < interface->num_altsetting;
                     alternate_index++) {
                    const struct libusb_interface_descriptor *alternate =
                        &interface->altsetting[alternate_index];
                    uint8_t endpoint = 0;
                    int packet_size = 0;
                    if (!is_audio_iso_out(device, alternate, &endpoint, &packet_size)) {
                        continue;
                    }
                    candidate->bus_number = libusb_get_bus_number(device);
                    candidate->device_address = libusb_get_device_address(device);
                    candidate->configuration = configuration->bConfigurationValue;
                    candidate->interface_number = alternate->bInterfaceNumber;
                    candidate->alternate = alternate->bAlternateSetting;
                    candidate->endpoint = endpoint;
                    candidate->packet_size = packet_size;
                    break;
                }
            }
            libusb_free_config_descriptor(configuration);
        }
    }
    libusb_free_device_list(devices, 1);

    if (candidate->packet_size == 0) {
        return failf("QEMU USB audio ISO OUT endpoint not found");
    }
    return 0;
}

static int ensure_configuration(libusb_context *context, const audio_endpoint_t *endpoint) {
    libusb_device_handle *handle = NULL;
    libusb_device **devices = NULL;
    ssize_t count = libusb_get_device_list(context, &devices);
    for (ssize_t index = 0; index < count; index++) {
        if (libusb_get_bus_number(devices[index]) == endpoint->bus_number &&
            libusb_get_device_address(devices[index]) == endpoint->device_address &&
            libusb_open(devices[index], &handle) == 0) {
            break;
        }
    }
    if (devices != NULL) {
        libusb_free_device_list(devices, 1);
    }
    if (handle == NULL) {
        return failf("failed to open QEMU USB audio device");
    }

    int active_configuration = 0;
    int result = libusb_get_configuration(handle, &active_configuration);
    if (result == 0 && active_configuration != endpoint->configuration) {
        result = libusb_set_configuration(handle, endpoint->configuration);
    }
    libusb_close(handle);
    if (result != 0) {
        return failf("set configuration %u: %s", endpoint->configuration, libusb_error_name(result));
    }
    return 0;
}

static iso_urb_t allocate_iso_urb(const audio_endpoint_t *endpoint, int packet_count) {
    iso_urb_t allocation = {0};
    size_t urb_size =
        sizeof(struct usbfs_urb) + (size_t)packet_count * sizeof(struct usbfs_iso_packet_desc);
    allocation.urb = calloc(1, urb_size);
    allocation.buffer = calloc((size_t)packet_count, (size_t)endpoint->packet_size);
    if (allocation.urb == NULL || allocation.buffer == NULL) {
        free(allocation.urb);
        free(allocation.buffer);
        return (iso_urb_t){0};
    }

    allocation.urb->type = USBFS_URB_TYPE_ISO;
    allocation.urb->endpoint = endpoint->endpoint;
    allocation.urb->flags = USBFS_URB_ISO_ASAP;
    allocation.urb->buffer = allocation.buffer;
    allocation.urb->buffer_length = packet_count * endpoint->packet_size;
    allocation.urb->number_of_packets = packet_count;
    for (int index = 0; index < packet_count; index++) {
        allocation.urb->iso_frame_desc[index].length = (unsigned int)endpoint->packet_size;
    }
    return allocation;
}

static void free_iso_urb(iso_urb_t *allocation) {
    free(allocation->urb);
    free(allocation->buffer);
    *allocation = (iso_urb_t){0};
}

static int set_alternate(int fd, const audio_endpoint_t *endpoint, uint8_t alternate) {
    struct usbfs_setinterface setting = {
        .interface = endpoint->interface_number,
        .altsetting = alternate,
    };
    if (ioctl(fd, USBFS_SETINTERFACE, &setting) < 0) {
        return failf(
            "SETINTERFACE if=%u alt=%u: errno=%d (%s)",
            endpoint->interface_number,
            alternate,
            errno,
            strerror(errno)
        );
    }
    return 0;
}

static int expect_no_completion(int fd, const char *stage) {
    void *completed = NULL;
    if (ioctl(fd, USBFS_REAPURBNDELAY, &completed) == 0) {
        return failf("%s unexpectedly completed URB %p", stage, completed);
    }
    if (errno != EAGAIN) {
        return failf("%s REAPURBNDELAY: errno=%d (%s)", stage, errno, strerror(errno));
    }
    return 0;
}

static int run_lifecycle_test(const audio_endpoint_t *endpoint) {
    char path[32];
    snprintf(path, sizeof(path), "/dev/bus/usb/%03u/%03u", endpoint->bus_number,
             endpoint->device_address);
    int fd = open(path, O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        return failf("open %s: errno=%d (%s)", path, errno, strerror(errno));
    }

    int interface = endpoint->interface_number;
    int result = 1;
    iso_urb_t quiesced = {0};
    iso_urb_t barrier = {0};
    bool claimed = false;

    if (ioctl(fd, USBFS_CLAIMINTERFACE, &interface) < 0) {
        failf("CLAIMINTERFACE %d: errno=%d (%s)", interface, errno, strerror(errno));
        goto cleanup;
    }
    claimed = true;
    if (set_alternate(fd, endpoint, endpoint->alternate) != 0) {
        goto cleanup;
    }

    quiesced = allocate_iso_urb(endpoint, QUIESCED_URB_PACKETS);
    if (quiesced.urb == NULL) {
        failf("allocate quiesced URB");
        goto cleanup;
    }
    if (ioctl(fd, USBFS_SUBMITURB, quiesced.urb) < 0) {
        failf("SUBMITURB quiesced: errno=%d (%s)", errno, strerror(errno));
        goto cleanup;
    }

    if (set_alternate(fd, endpoint, 0) != 0) {
        goto cleanup;
    }
    if (expect_no_completion(fd, "after endpoint quiesce") != 0) {
        goto cleanup;
    }

    if (set_alternate(fd, endpoint, endpoint->alternate) != 0) {
        goto cleanup;
    }
    /* Completion of this request proves that xHCI has accepted work on the
     * replacement endpoint. It is an ordering barrier, not a grace period. */
    barrier = allocate_iso_urb(endpoint, BARRIER_URB_PACKETS);
    if (barrier.urb == NULL) {
        failf("allocate barrier URB");
        goto cleanup;
    }
    if (ioctl(fd, USBFS_SUBMITURB, barrier.urb) < 0) {
        failf("SUBMITURB barrier: errno=%d (%s)", errno, strerror(errno));
        goto cleanup;
    }

    void *completed = NULL;
    if (ioctl(fd, USBFS_REAPURB, &completed) < 0) {
        failf("REAPURB barrier: errno=%d (%s)", errno, strerror(errno));
        goto cleanup;
    }
    if (completed != barrier.urb) {
        failf("expected barrier URB %p, got stale completion %p", (void *)barrier.urb, completed);
        goto cleanup;
    }
    if (expect_no_completion(fd, "after barrier completion") != 0) {
        goto cleanup;
    }

    puts("usbfs alternate URB lifecycle passed: quiesced request retired once, no stale completion");
    result = 0;

cleanup:
    if (claimed) {
        (void)set_alternate(fd, endpoint, 0);
        (void)ioctl(fd, USBFS_RELEASEINTERFACE, &interface);
    }
    close(fd);
    free_iso_urb(&barrier);
    free_iso_urb(&quiesced);
    return result;
}

int main(void) {
#if defined(__loongarch__) || defined(__loongarch64)
    puts("usbfs alternate URB lifecycle test skipped without QEMU xHCI");
    return 0;
#endif

    libusb_context *context = NULL;
    int result = libusb_init(&context);
    if (result != 0) {
        return failf("libusb_init: %s", libusb_error_name(result));
    }

    audio_endpoint_t endpoint;
    result = find_audio_endpoint(context, &endpoint);
    if (result == 0) {
        result = ensure_configuration(context, &endpoint);
    }
    if (result == 0) {
        printf(
            "usbfs lifecycle device: bus=%u device=%u if=%u alt=%u ep=%02x packet=%d\n",
            endpoint.bus_number,
            endpoint.device_address,
            endpoint.interface_number,
            endpoint.alternate,
            endpoint.endpoint,
            endpoint.packet_size
        );
        result = run_lifecycle_test(&endpoint);
    }
    libusb_exit(context);
    return result;
}
