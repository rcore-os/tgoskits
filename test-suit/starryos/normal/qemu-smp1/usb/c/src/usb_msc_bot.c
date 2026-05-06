#include "usb_msc_bot.h"

#include <arpa/inet.h>
#include <stdbool.h>
#include <stdio.h>
#include <string.h>

#define USB_MSC_CLASS 0x08
#define USB_MSC_SUBCLASS_SCSI 0x06
#define USB_MSC_PROTOCOL_BULK_ONLY 0x50
#define USB_MSC_TIMEOUT_MS 10000
#define CBW_SIGNATURE 0x43425355u
#define CSW_SIGNATURE 0x53425355u

struct __attribute__((packed)) usb_msc_cbw {
    uint32_t signature;
    uint32_t tag;
    uint32_t transfer_length;
    uint8_t flags;
    uint8_t lun;
    uint8_t command_length;
    uint8_t command_block[16];
};

struct __attribute__((packed)) usb_msc_csw {
    uint32_t signature;
    uint32_t tag;
    uint32_t residue;
    uint8_t status;
};

static int find_bulk_endpoints(
    const struct libusb_interface_descriptor *descriptor,
    uint8_t *endpoint_in,
    uint8_t *endpoint_out
) {
    *endpoint_in = 0;
    *endpoint_out = 0;

    for (int index = 0; index < descriptor->bNumEndpoints; index++) {
        const struct libusb_endpoint_descriptor *endpoint =
            &descriptor->endpoint[index];

        if ((endpoint->bmAttributes & LIBUSB_TRANSFER_TYPE_MASK) !=
            LIBUSB_TRANSFER_TYPE_BULK) {
            continue;
        }

        if ((endpoint->bEndpointAddress & LIBUSB_ENDPOINT_DIR_MASK) ==
            LIBUSB_ENDPOINT_IN) {
            *endpoint_in = endpoint->bEndpointAddress;
        } else {
            *endpoint_out = endpoint->bEndpointAddress;
        }
    }

    return (*endpoint_in != 0 && *endpoint_out != 0) ? 0 : -1;
}

static int prepare_bulk_only_device(
    libusb_device *usb_device,
    const struct libusb_interface_descriptor *descriptor,
    usb_msc_device_t *device
) {
    int result = libusb_open(usb_device, &device->handle);
    if (result != 0) {
        printf("usb_msc: libusb_open failed (%d, %s)\n", result, libusb_error_name(result));
        fflush(stdout);
        return result;
    }

    device->interface_number = descriptor->bInterfaceNumber;
    result = find_bulk_endpoints(
        descriptor,
        &device->endpoint_in,
        &device->endpoint_out
    );
    if (result != 0) {
        printf("usb_msc: missing bulk endpoints (%d)\n", result);
        fflush(stdout);
        libusb_close(device->handle);
        device->handle = NULL;
        return LIBUSB_ERROR_NOT_FOUND;
    }

    result = libusb_claim_interface(device->handle, device->interface_number);
    if (result != 0) {
        printf(
            "usb_msc: libusb_claim_interface(%u) failed (%d, %s)\n",
            device->interface_number,
            result,
            libusb_error_name(result)
        );
        fflush(stdout);
        libusb_close(device->handle);
        device->handle = NULL;
        return result;
    }

    device->tag = 1;
    return 0;
}

int usb_msc_find_and_open(usb_msc_device_t *device) {
    libusb_device **device_list = NULL;
    ssize_t count = 0;

    memset(device, 0, sizeof(*device));
    int result = libusb_init(&device->ctx);
    if (result != 0) {
        printf("usb_msc: libusb_init failed (%d, %s)\n", result, libusb_error_name(result));
        fflush(stdout);
        return result;
    }

    count = libusb_get_device_list(device->ctx, &device_list);
    if (count < 0) {
        printf(
            "usb_msc: libusb_get_device_list failed (%zd, %s)\n",
            count,
            libusb_error_name((int)count)
        );
        fflush(stdout);
        libusb_exit(device->ctx);
        device->ctx = NULL;
        return (int)count;
    }

    for (ssize_t dev_index = 0; dev_index < count; dev_index++) {
        libusb_device *usb_device = device_list[dev_index];
        struct libusb_device_descriptor descriptor;

        result = libusb_get_device_descriptor(usb_device, &descriptor);
        if (result != 0) {
            printf(
                "usb_msc: libusb_get_device_descriptor failed (%d, %s)\n",
                result,
                libusb_error_name(result)
            );
            fflush(stdout);
            continue;
        }

        printf(
            "usb device: bus=%u addr=%u vid=%04x pid=%04x class=%02x\n",
            libusb_get_bus_number(usb_device),
            libusb_get_device_address(usb_device),
            descriptor.idVendor,
            descriptor.idProduct,
            descriptor.bDeviceClass
        );

        struct libusb_config_descriptor *config = NULL;
        result = libusb_get_active_config_descriptor(usb_device, &config);
        if (result == LIBUSB_ERROR_NOT_FOUND) {
            result = libusb_get_config_descriptor(usb_device, 0, &config);
        }
        if (result != 0 || config == NULL) {
            printf(
                "usb_msc: config descriptor lookup failed (%d, %s)\n",
                result,
                libusb_error_name(result)
            );
            fflush(stdout);
            continue;
        }

        bool found = false;
        for (int interface_index = 0;
             interface_index < config->bNumInterfaces && !found;
             interface_index++) {
            const struct libusb_interface *interface = &config->interface[interface_index];
            for (int alt_index = 0;
                 alt_index < interface->num_altsetting && !found;
                 alt_index++) {
                const struct libusb_interface_descriptor *if_desc =
                    &interface->altsetting[alt_index];
                if (if_desc->bInterfaceClass != USB_MSC_CLASS ||
                    if_desc->bInterfaceSubClass != USB_MSC_SUBCLASS_SCSI ||
                    if_desc->bInterfaceProtocol != USB_MSC_PROTOCOL_BULK_ONLY) {
                    continue;
                }

                result = prepare_bulk_only_device(usb_device, if_desc, device);
                if (result == 0) {
                    found = true;
                } else {
                    printf(
                        "usb_msc: prepare device failed (%d, %s)\n",
                        result,
                        libusb_error_name(result)
                    );
                    fflush(stdout);
                }
            }
        }

        libusb_free_config_descriptor(config);
        if (found) {
            libusb_free_device_list(device_list, 1);
            return 0;
        }
    }

    libusb_free_device_list(device_list, 1);
    usb_msc_close(device);
    return LIBUSB_ERROR_NOT_FOUND;
}

void usb_msc_close(usb_msc_device_t *device) {
    if (device->handle != NULL) {
        libusb_release_interface(device->handle, device->interface_number);
        libusb_close(device->handle);
        device->handle = NULL;
    }
    if (device->ctx != NULL) {
        libusb_exit(device->ctx);
        device->ctx = NULL;
    }
}

static int transfer_command(
    usb_msc_device_t *device,
    const uint8_t *command,
    uint8_t command_length,
    uint8_t direction_in,
    void *data,
    uint32_t transfer_length
) {
    struct usb_msc_cbw cbw;
    struct usb_msc_csw csw;
    int transferred = 0;

    memset(&cbw, 0, sizeof(cbw));
    cbw.signature = CBW_SIGNATURE;
    cbw.tag = device->tag++;
    cbw.transfer_length = transfer_length;
    cbw.flags = direction_in;
    cbw.lun = 0;
    cbw.command_length = command_length;
    memcpy(cbw.command_block, command, command_length);

    int result = libusb_bulk_transfer(
        device->handle,
        device->endpoint_out,
        (unsigned char *)&cbw,
        (int)sizeof(cbw),
        &transferred,
        USB_MSC_TIMEOUT_MS
    );
    if (result != 0 || transferred != (int)sizeof(cbw)) {
        printf(
            "usb_msc: CBW OUT failed result=%d transferred=%d expected=%zu\n",
            result,
            transferred,
            sizeof(cbw)
        );
        fflush(stdout);
        return result != 0 ? result : LIBUSB_ERROR_IO;
    }

    if (transfer_length > 0) {
        const unsigned char endpoint =
            direction_in == LIBUSB_ENDPOINT_IN ? device->endpoint_in : device->endpoint_out;
        result = libusb_bulk_transfer(
            device->handle,
            endpoint,
            (unsigned char *)data,
            (int)transfer_length,
            &transferred,
            USB_MSC_TIMEOUT_MS
        );
        if (result != 0 || transferred != (int)transfer_length) {
            printf(
                "usb_msc: DATA %s failed result=%d transferred=%d expected=%u\n",
                direction_in == LIBUSB_ENDPOINT_IN ? "IN" : "OUT",
                result,
                transferred,
                transfer_length
            );
            fflush(stdout);
            return result != 0 ? result : LIBUSB_ERROR_IO;
        }
    }

    result = libusb_bulk_transfer(
        device->handle,
        device->endpoint_in,
        (unsigned char *)&csw,
        (int)sizeof(csw),
        &transferred,
        USB_MSC_TIMEOUT_MS
    );
    if (result != 0 || transferred != (int)sizeof(csw)) {
        printf(
            "usb_msc: CSW IN failed result=%d transferred=%d expected=%zu\n",
            result,
            transferred,
            sizeof(csw)
        );
        fflush(stdout);
        return result != 0 ? result : LIBUSB_ERROR_IO;
    }

    if (csw.signature != CSW_SIGNATURE || csw.tag != cbw.tag) {
        printf(
            "usb_msc: CSW signature/tag mismatch sig=%08x tag=%08x expected_sig=%08x expected_tag=%08x\n",
            csw.signature,
            csw.tag,
            CSW_SIGNATURE,
            cbw.tag
        );
        fflush(stdout);
        return LIBUSB_ERROR_IO;
    }
    if (csw.status != 0) {
        printf("usb_msc: CSW status=%u residue=%u\n", csw.status, csw.residue);
        fflush(stdout);
        return LIBUSB_ERROR_IO;
    }

    return 0;
}

int usb_msc_inquiry(usb_msc_device_t *device, char vendor[9], char product[17]) {
    uint8_t command[6] = {0x12, 0, 0, 0, 36, 0};
    uint8_t response[36];
    int result = transfer_command(
        device,
        command,
        sizeof(command),
        LIBUSB_ENDPOINT_IN,
        response,
        sizeof(response)
    );
    if (result != 0) {
        return result;
    }

    memcpy(vendor, &response[8], 8);
    vendor[8] = '\0';
    memcpy(product, &response[16], 16);
    product[16] = '\0';
    return 0;
}

int usb_msc_test_unit_ready(usb_msc_device_t *device) {
    uint8_t command[6] = {0x00, 0, 0, 0, 0, 0};
    return transfer_command(
        device,
        command,
        sizeof(command),
        LIBUSB_ENDPOINT_IN,
        NULL,
        0
    );
}

int usb_msc_request_sense(
    usb_msc_device_t *device,
    uint8_t *sense_key,
    uint8_t *asc,
    uint8_t *ascq
) {
    uint8_t command[6] = {0x03, 0, 0, 0, 18, 0};
    uint8_t response[18];
    int result = transfer_command(
        device,
        command,
        sizeof(command),
        LIBUSB_ENDPOINT_IN,
        response,
        sizeof(response)
    );
    if (result != 0) {
        return result;
    }

    if (sense_key != NULL) {
        *sense_key = response[2] & 0x0f;
    }
    if (asc != NULL) {
        *asc = response[12];
    }
    if (ascq != NULL) {
        *ascq = response[13];
    }
    return 0;
}

int usb_msc_read_capacity10(
    usb_msc_device_t *device,
    uint32_t *last_lba,
    uint32_t *block_size
) {
    uint8_t command[10] = {0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0};
    uint8_t response[8];
    int result = transfer_command(
        device,
        command,
        sizeof(command),
        LIBUSB_ENDPOINT_IN,
        response,
        sizeof(response)
    );
    if (result != 0) {
        return result;
    }

    *last_lba = ntohl(*(uint32_t *)&response[0]);
    *block_size = ntohl(*(uint32_t *)&response[4]);
    return 0;
}

int usb_msc_read10(
    usb_msc_device_t *device,
    uint32_t lba,
    uint16_t blocks,
    void *buffer,
    uint32_t transfer_length
) {
    uint8_t command[10] = {0x28, 0, 0, 0, 0, 0, 0, 0, 0, 0};
    const uint32_t be_lba = htonl(lba);
    const uint16_t be_blocks = htons(blocks);
    memcpy(&command[2], &be_lba, sizeof(be_lba));
    memcpy(&command[7], &be_blocks, sizeof(be_blocks));

    return transfer_command(
        device,
        command,
        sizeof(command),
        LIBUSB_ENDPOINT_IN,
        buffer,
        transfer_length
    );
}

int usb_msc_write10(
    usb_msc_device_t *device,
    uint32_t lba,
    uint16_t blocks,
    const void *buffer,
    uint32_t transfer_length
) {
    uint8_t command[10] = {0x2a, 0, 0, 0, 0, 0, 0, 0, 0, 0};
    const uint32_t be_lba = htonl(lba);
    const uint16_t be_blocks = htons(blocks);
    memcpy(&command[2], &be_lba, sizeof(be_lba));
    memcpy(&command[7], &be_blocks, sizeof(be_blocks));

    return transfer_command(
        device,
        command,
        sizeof(command),
        0,
        (void *)buffer,
        transfer_length
    );
}
