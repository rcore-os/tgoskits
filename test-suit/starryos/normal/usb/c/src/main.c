#include "usb_msc_bot.h"

#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

#define USB_MSC_CLASS 0x08
#define USB_MSC_SUBCLASS_SCSI 0x06
#define USB_MSC_PROTOCOL_BULK_ONLY 0x50
#define MAX_ENUM_RETRIES 5

static int failf(const char *format, ...) {
    va_list args;
    va_start(args, format);
    fputs("USB test failed: ", stdout);
    vprintf(format, args);
    fputc('\n', stdout);
    va_end(args);
    return 1;
}

int main(void) {
    libusb_context *ctx = NULL;
    libusb_device **device_list = NULL;
    ssize_t count = 0;
    int exit_code = 1;
    bool found = false;

    int result = libusb_init(&ctx);
    if (result != 0) {
        return failf("libusb init failed (%d, %s)", result, libusb_error_name(result));
    }

    for (int attempt = 1; attempt <= MAX_ENUM_RETRIES && !found; attempt++) {
        count = libusb_get_device_list(ctx, &device_list);
        if (count < 0) {
            libusb_exit(ctx);
            return failf(
                "libusb_get_device_list failed (%zd, %s)",
                count,
                libusb_error_name((int)count)
            );
        }

        printf("usb device count (attempt %d/%d): %zd\n", attempt, MAX_ENUM_RETRIES, count);

        for (ssize_t dev_index = 0; dev_index < count && !found; dev_index++) {
            libusb_device *device = device_list[dev_index];
            struct libusb_device_descriptor descriptor;

            result = libusb_get_device_descriptor(device, &descriptor);
            if (result != 0) {
                continue;
            }

            printf(
                "usb device: bus=%u addr=%u vid=%04x pid=%04x configs=%u\n",
                libusb_get_bus_number(device),
                libusb_get_device_address(device),
                descriptor.idVendor,
                descriptor.idProduct,
                descriptor.bNumConfigurations
            );

            for (uint8_t config_index = 0;
                 config_index < descriptor.bNumConfigurations && !found;
                 config_index++) {
                struct libusb_config_descriptor *config = NULL;
                result = libusb_get_config_descriptor(device, config_index, &config);
                if (result != 0 || config == NULL) {
                    libusb_free_device_list(device_list, 1);
                    libusb_exit(ctx);
                    return failf(
                        "libusb_get_config_descriptor failed (%d, %s)",
                        result,
                        libusb_error_name(result)
                    );
                }

                printf(
                    "usb config: value=%u interfaces=%u\n",
                    config->bConfigurationValue,
                    config->bNumInterfaces
                );

                for (int interface_index = 0;
                     interface_index < config->bNumInterfaces && !found;
                     interface_index++) {
                    const struct libusb_interface *interface = &config->interface[interface_index];
                    for (int alt_index = 0;
                         alt_index < interface->num_altsetting && !found;
                         alt_index++) {
                        const struct libusb_interface_descriptor *if_desc =
                            &interface->altsetting[alt_index];
                        printf(
                            "usb interface: num=%u alt=%u class=%02x subclass=%02x protocol=%02x eps=%u\n",
                            if_desc->bInterfaceNumber,
                            if_desc->bAlternateSetting,
                            if_desc->bInterfaceClass,
                            if_desc->bInterfaceSubClass,
                            if_desc->bInterfaceProtocol,
                            if_desc->bNumEndpoints
                        );
                        if (if_desc->bInterfaceClass == USB_MSC_CLASS &&
                            if_desc->bInterfaceSubClass == USB_MSC_SUBCLASS_SCSI &&
                            if_desc->bInterfaceProtocol == USB_MSC_PROTOCOL_BULK_ONLY) {
                            found = true;
                        }
                    }
                }

                libusb_free_config_descriptor(config);
            }
        }

        libusb_free_device_list(device_list, 1);
        device_list = NULL;
        if (!found) {
            sleep(1);
        }
    }

    if (!found) {
        libusb_exit(ctx);
        return failf("no USB mass-storage bulk-only interface found during enumeration");
    }

    libusb_exit(ctx);
    puts("USB enumeration tests passed!");
    exit_code = 0;
    return exit_code;
}
