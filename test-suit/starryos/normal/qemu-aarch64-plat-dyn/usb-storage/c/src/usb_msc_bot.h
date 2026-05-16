#ifndef STARRY_USB_MSC_BOT_H
#define STARRY_USB_MSC_BOT_H

#include <libusb.h>
#include <stdint.h>

typedef struct usb_msc_device {
    libusb_context *ctx;
    libusb_device_handle *handle;
    uint8_t interface_number;
    uint8_t endpoint_in;
    uint8_t endpoint_out;
    uint32_t tag;
} usb_msc_device_t;

int usb_msc_find_and_open(usb_msc_device_t *device);
void usb_msc_close(usb_msc_device_t *device);

int usb_msc_inquiry(usb_msc_device_t *device, char vendor[9], char product[17]);
int usb_msc_test_unit_ready(usb_msc_device_t *device);
int usb_msc_request_sense(
    usb_msc_device_t *device,
    uint8_t *sense_key,
    uint8_t *asc,
    uint8_t *ascq
);
int usb_msc_read_capacity10(
    usb_msc_device_t *device,
    uint32_t *last_lba,
    uint32_t *block_size
);
int usb_msc_read10(
    usb_msc_device_t *device,
    uint32_t lba,
    uint16_t blocks,
    void *buffer,
    uint32_t transfer_length
);
int usb_msc_write10(
    usb_msc_device_t *device,
    uint32_t lba,
    uint16_t blocks,
    const void *buffer,
    uint32_t transfer_length
);

#endif
