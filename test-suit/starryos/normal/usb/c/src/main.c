#include "usb_msc_bot.h"

#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define USB_MSC_CLASS 0x08
#define USB_MSC_SUBCLASS_SCSI 0x06
#define USB_MSC_PROTOCOL_BULK_ONLY 0x50
#define MAX_ENUM_RETRIES 5
#define MAX_UNIT_READY_RETRIES 5
#define BULK_TRANSFER_TARGET_BYTES (16u * 1024u)

static int failf(const char *format, ...) {
    va_list args;
    va_start(args, format);
    fputs("USB test failed: ", stdout);
    vprintf(format, args);
    fputc('\n', stdout);
    va_end(args);
    return 1;
}

static int find_bulk_endpoints(
    const struct libusb_interface_descriptor *descriptor,
    uint8_t *endpoint_in,
    uint8_t *endpoint_out
) {
    *endpoint_in = 0;
    *endpoint_out = 0;

    for (int index = 0; index < descriptor->bNumEndpoints; index++) {
        const struct libusb_endpoint_descriptor *endpoint = &descriptor->endpoint[index];
        if ((endpoint->bmAttributes & LIBUSB_TRANSFER_TYPE_MASK) != LIBUSB_TRANSFER_TYPE_BULK) {
            continue;
        }

        if ((endpoint->bEndpointAddress & LIBUSB_ENDPOINT_DIR_MASK) == LIBUSB_ENDPOINT_IN) {
            *endpoint_in = endpoint->bEndpointAddress;
        } else {
            *endpoint_out = endpoint->bEndpointAddress;
        }
    }

    return (*endpoint_in != 0 && *endpoint_out != 0) ? 0 : -1;
}

static void fill_test_pattern(
    uint8_t *buffer,
    size_t length,
    uint32_t lba,
    uint32_t block_size
) {
    for (size_t offset = 0; offset < length; offset++) {
        const uint32_t block_index = block_size == 0 ? 0 : (uint32_t)(offset / block_size);
        buffer[offset] = (uint8_t)((0x5au + (offset * 17u) + block_index + (lba & 0xffu)) & 0xffu);
    }

    if (length >= 16) {
        memcpy(buffer, "STARRY-USB-BULK", 15);
        buffer[15] = (uint8_t)(lba & 0xffu);
    }
}

static int wait_until_unit_ready(usb_msc_device_t *device) {
    int result = 0;

    for (int attempt = 1; attempt <= MAX_UNIT_READY_RETRIES; attempt++) {
        result = usb_msc_test_unit_ready(device);
        if (result == 0) {
            return 0;
        }

        uint8_t sense_key = 0;
        uint8_t asc = 0;
        uint8_t ascq = 0;
        const int sense_result = usb_msc_request_sense(device, &sense_key, &asc, &ascq);
        if (sense_result == 0) {
            printf(
                "usb TUR retry %d/%d: result=%d sense=%02x/%02x/%02x\n",
                attempt,
                MAX_UNIT_READY_RETRIES,
                result,
                sense_key,
                asc,
                ascq
            );
        } else {
            printf(
                "usb TUR retry %d/%d: result=%d request_sense=%d (%s)\n",
                attempt,
                MAX_UNIT_READY_RETRIES,
                result,
                sense_result,
                libusb_error_name(sense_result)
            );
        }
        fflush(stdout);
        sleep(1);
    }

    return result;
}

static int run_mass_storage_bulk_test(usb_msc_device_t *device) {
    char vendor[9];
    char product[17];
    uint32_t last_lba = 0;
    uint32_t block_size = 0;
    uint32_t blocks = 0;
    uint32_t test_lba = 0;
    uint16_t blocks16 = 0;
    size_t transfer_length = 0;
    uint8_t *original = NULL;
    uint8_t *write_buffer = NULL;
    uint8_t *readback = NULL;
    int exit_code = 1;

    memset(vendor, 0, sizeof(vendor));
    memset(product, 0, sizeof(product));

    int result = usb_msc_inquiry(device, vendor, product);
    if (result != 0) {
        exit_code = failf("usb_msc_inquiry failed (%d, %s)", result, libusb_error_name(result));
        goto cleanup;
    }
    printf("usb inquiry: vendor=\"%s\" product=\"%s\"\n", vendor, product);
    fflush(stdout);

    result = wait_until_unit_ready(device);
    if (result != 0) {
        exit_code = failf(
            "usb_msc_test_unit_ready failed after retries (%d, %s)",
            result,
            libusb_error_name(result)
        );
        goto cleanup;
    }

    result = usb_msc_read_capacity10(device, &last_lba, &block_size);
    if (result != 0) {
        exit_code = failf(
            "usb_msc_read_capacity10 failed (%d, %s)",
            result,
            libusb_error_name(result)
        );
        goto cleanup;
    }
    if (block_size == 0) {
        exit_code = failf("READ CAPACITY returned block_size=0");
        goto cleanup;
    }

    blocks = (BULK_TRANSFER_TARGET_BYTES / block_size) + 1u;
    if (blocks == 0) {
        blocks = 1;
    }
    if (blocks > UINT16_MAX) {
        exit_code = failf("bulk test requires too many blocks: %u", blocks);
        goto cleanup;
    }
    if ((uint64_t)block_size * (uint64_t)blocks > SIZE_MAX) {
        exit_code = failf("bulk transfer length overflows size_t");
        goto cleanup;
    }
    transfer_length = (size_t)block_size * (size_t)blocks;

    const uint64_t total_blocks = (uint64_t)last_lba + 1u;
    if (total_blocks <= (uint64_t)blocks + 2u) {
        exit_code = failf(
            "USB medium too small: total_blocks=%llu requested=%u",
            (unsigned long long)total_blocks,
            blocks
        );
        goto cleanup;
    }

    test_lba = (uint32_t)(total_blocks / 2u);
    if ((uint64_t)test_lba + (uint64_t)blocks > total_blocks) {
        test_lba = (uint32_t)(total_blocks - (uint64_t)blocks);
    }
    if (test_lba == 0) {
        test_lba = 1;
    }
    if ((uint64_t)test_lba + (uint64_t)blocks > total_blocks) {
        exit_code = failf(
            "unable to choose a safe test LBA: lba=%u blocks=%u total=%llu",
            test_lba,
            blocks,
            (unsigned long long)total_blocks
        );
        goto cleanup;
    }
    blocks16 = (uint16_t)blocks;

    printf(
        "usb capacity: last_lba=%u block_size=%u test_lba=%u blocks=%u bytes=%zu\n",
        last_lba,
        block_size,
        test_lba,
        blocks,
        transfer_length
    );
    fflush(stdout);

    original = malloc(transfer_length);
    write_buffer = malloc(transfer_length);
    readback = malloc(transfer_length);
    if (original == NULL || write_buffer == NULL || readback == NULL) {
        exit_code = failf("failed to allocate %zu-byte bulk buffers", transfer_length);
        goto cleanup;
    }

    result = usb_msc_read10(device, test_lba, blocks16, original, (uint32_t)transfer_length);
    if (result != 0) {
        exit_code = failf("usb_msc_read10(original) failed (%d, %s)", result, libusb_error_name(result));
        goto cleanup;
    }

    fill_test_pattern(write_buffer, transfer_length, test_lba, block_size);
    result = usb_msc_write10(device, test_lba, blocks16, write_buffer, (uint32_t)transfer_length);
    if (result != 0) {
        exit_code = failf("usb_msc_write10 failed (%d, %s)", result, libusb_error_name(result));
        goto cleanup;
    }

    memset(readback, 0, transfer_length);
    result = usb_msc_read10(device, test_lba, blocks16, readback, (uint32_t)transfer_length);
    if (result != 0) {
        exit_code = failf("usb_msc_read10(readback) failed (%d, %s)", result, libusb_error_name(result));
        goto cleanup;
    }
    if (memcmp(write_buffer, readback, transfer_length) != 0) {
        exit_code = failf(
            "bulk readback mismatch at lba=%u blocks=%u bytes=%zu",
            test_lba,
            blocks,
            transfer_length
        );
        goto cleanup;
    }

    printf(
        "usb bulk readback ok: lba=%u blocks=%u bytes=%zu\n",
        test_lba,
        blocks,
        transfer_length
    );
    fflush(stdout);

    result = usb_msc_write10(device, test_lba, blocks16, original, (uint32_t)transfer_length);
    if (result != 0) {
        exit_code = failf("usb_msc_write10(restore) failed (%d, %s)", result, libusb_error_name(result));
        goto cleanup;
    }

    memset(readback, 0, transfer_length);
    result = usb_msc_read10(device, test_lba, blocks16, readback, (uint32_t)transfer_length);
    if (result != 0) {
        exit_code = failf("usb_msc_read10(restore verify) failed (%d, %s)", result, libusb_error_name(result));
        goto cleanup;
    }
    if (memcmp(original, readback, transfer_length) != 0) {
        exit_code = failf(
            "restored data mismatch at lba=%u blocks=%u bytes=%zu",
            test_lba,
            blocks,
            transfer_length
        );
        goto cleanup;
    }

    printf(
        "usb bulk restore ok: lba=%u blocks=%u bytes=%zu\n",
        test_lba,
        blocks,
        transfer_length
    );
    fflush(stdout);

    exit_code = 0;

cleanup:
    free(readback);
    free(write_buffer);
    free(original);
    return exit_code;
}

int main(void) {
    libusb_context *ctx = NULL;
    libusb_device **device_list = NULL;
    ssize_t count = 0;
    int exit_code = 1;
    bool found = false;
    bool opened = false;

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
        fflush(stdout);

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
            fflush(stdout);

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
                fflush(stdout);

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
                        fflush(stdout);
                        if (if_desc->bInterfaceClass == USB_MSC_CLASS &&
                            if_desc->bInterfaceSubClass == USB_MSC_SUBCLASS_SCSI &&
                            if_desc->bInterfaceProtocol == USB_MSC_PROTOCOL_BULK_ONLY) {
                            libusb_device_handle *handle = NULL;
                            usb_msc_device_t msc_device;
                            unsigned char ctrl_desc[LIBUSB_DT_DEVICE_SIZE];
                            uint8_t endpoint_in = 0;
                            uint8_t endpoint_out = 0;

                            result = libusb_open(device, &handle);
                            if (result != 0 || handle == NULL) {
                                libusb_free_config_descriptor(config);
                                libusb_free_device_list(device_list, 1);
                                libusb_exit(ctx);
                                return failf(
                                    "libusb_open failed (%d, %s)",
                                    result,
                                    libusb_error_name(result)
                                );
                            }
                            if (find_bulk_endpoints(if_desc, &endpoint_in, &endpoint_out) != 0) {
                                libusb_close(handle);
                                libusb_free_config_descriptor(config);
                                libusb_free_device_list(device_list, 1);
                                libusb_exit(ctx);
                                return failf("failed to find bulk endpoints on the mass-storage interface");
                            }

                            result = libusb_control_transfer(
                                handle,
                                LIBUSB_ENDPOINT_IN | LIBUSB_REQUEST_TYPE_STANDARD |
                                    LIBUSB_RECIPIENT_DEVICE,
                                LIBUSB_REQUEST_GET_DESCRIPTOR,
                                (uint16_t)(LIBUSB_DT_DEVICE << 8),
                                0,
                                ctrl_desc,
                                (uint16_t)sizeof(ctrl_desc),
                                1000
                            );
                            if (result != (int)sizeof(ctrl_desc)) {
                                libusb_close(handle);
                                libusb_free_config_descriptor(config);
                                libusb_free_device_list(device_list, 1);
                                libusb_exit(ctx);
                                return failf(
                                    "libusb_control_transfer returned %d instead of %zu",
                                    result,
                                    sizeof(ctrl_desc)
                                );
                            }

                            if (memcmp(ctrl_desc + 8, &descriptor.idVendor, sizeof(descriptor.idVendor)) != 0 ||
                                memcmp(ctrl_desc + 10, &descriptor.idProduct, sizeof(descriptor.idProduct)) != 0 ||
                                ctrl_desc[17] != descriptor.bNumConfigurations) {
                                libusb_close(handle);
                                libusb_free_config_descriptor(config);
                                libusb_free_device_list(device_list, 1);
                                libusb_exit(ctx);
                                return failf("device descriptor from control transfer did not match enumeration");
                            }

                            printf(
                                "usb control descriptor ok: vid=%04x pid=%04x configs=%u\n",
                                descriptor.idVendor,
                                descriptor.idProduct,
                                descriptor.bNumConfigurations
                            );

                            result = libusb_claim_interface(handle, if_desc->bInterfaceNumber);
                            if (result != 0) {
                                libusb_close(handle);
                                libusb_free_config_descriptor(config);
                                libusb_free_device_list(device_list, 1);
                                libusb_exit(ctx);
                                return failf(
                                    "libusb_claim_interface(%u) failed (%d, %s)",
                                    if_desc->bInterfaceNumber,
                                    result,
                                    libusb_error_name(result)
                                );
                            }

                            memset(&msc_device, 0, sizeof(msc_device));
                            msc_device.handle = handle;
                            msc_device.interface_number = if_desc->bInterfaceNumber;
                            msc_device.endpoint_in = endpoint_in;
                            msc_device.endpoint_out = endpoint_out;
                            msc_device.tag = 1;

                            result = run_mass_storage_bulk_test(&msc_device);
                            usb_msc_close(&msc_device);
                            if (result != 0) {
                                libusb_free_config_descriptor(config);
                                libusb_free_device_list(device_list, 1);
                                libusb_exit(ctx);
                                return result;
                            }

                            opened = true;
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
    if (!opened) {
        libusb_exit(ctx);
        return failf("mass-storage device was found but open/control validation did not run");
    }

    libusb_exit(ctx);
    puts("USB open/control/bulk tests passed!");
    exit_code = 0;
    return exit_code;
}
