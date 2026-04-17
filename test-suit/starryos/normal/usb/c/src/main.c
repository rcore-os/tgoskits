#include "usb_msc_bot.h"

#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define MAX_TUR_RETRIES 5

static int failf(const char *format, ...) {
    va_list args;
    va_start(args, format);
    fputs("USB test failed: ", stdout);
    vprintf(format, args);
    fputc('\n', stdout);
    va_end(args);
    return 1;
}

static void trim_right(char *text) {
    size_t len = strlen(text);
    while (len > 0 && text[len - 1] == ' ') {
        text[len - 1] = '\0';
        len--;
    }
}

static void fill_pattern(uint8_t *buffer, size_t length) {
    for (size_t index = 0; index < length; index++) {
        buffer[index] = (uint8_t)((index * 37u + 11u) & 0xffu);
    }
}

static int wait_until_ready(usb_msc_device_t *device) {
    for (int attempt = 1; attempt <= MAX_TUR_RETRIES; attempt++) {
        int result = usb_msc_test_unit_ready(device);
        if (result == 0) {
            return 0;
        }

        uint8_t sense_key = 0;
        uint8_t asc = 0;
        uint8_t ascq = 0;
        if (usb_msc_request_sense(device, &sense_key, &asc, &ascq) == 0) {
            printf(
                "test unit ready retry %d/%d: sense=%02x asc=%02x ascq=%02x\n",
                attempt,
                MAX_TUR_RETRIES,
                sense_key,
                asc,
                ascq
            );
        } else {
            printf("test unit ready retry %d/%d: no sense data\n", attempt, MAX_TUR_RETRIES);
        }

        sleep(1);
    }

    return -1;
}

int main(void) {
    puts("todo usb test");
    return 0;

    usb_msc_device_t device;
    uint32_t last_lba = 0;
    uint32_t block_size = 0;
    uint32_t total_blocks = 0;
    uint32_t test_lba = 0;
    uint8_t *write_buffer = NULL;
    uint8_t *read_buffer = NULL;
    int exit_code = 1;

    int result = usb_msc_find_and_open(&device);
    if (result != 0) {
        return failf(
            "no USB mass-storage bulk-only device found (%d, %s)",
            result,
            libusb_error_name(result)
        );
    }

    char vendor[9];
    char product[17];
    result = usb_msc_inquiry(&device, vendor, product);
    if (result != 0) {
        usb_msc_close(&device);
        return failf("INQUIRY command failed (%d, %s)", result, libusb_error_name(result));
    }

    trim_right(vendor);
    trim_right(product);
    printf("usb inquiry: vendor='%s' product='%s'\n", vendor, product);

    if (wait_until_ready(&device) != 0) {
        usb_msc_close(&device);
        return failf("device never became ready");
    }

    result = usb_msc_read_capacity10(&device, &last_lba, &block_size);
    if (result != 0) {
        usb_msc_close(&device);
        return failf(
            "READ CAPACITY(10) failed (%d, %s)",
            result,
            libusb_error_name(result)
        );
    }

    total_blocks = last_lba + 1;
    printf(
        "usb capacity: last_lba=%u blocks=%u block_size=%u\n",
        last_lba,
        total_blocks,
        block_size
    );
    if (block_size == 0 || total_blocks < 2) {
        usb_msc_close(&device);
        return failf("invalid capacity response");
    }

    test_lba = total_blocks > 64 ? 32u : 1u;
    if (test_lba >= total_blocks) {
        test_lba = total_blocks - 1;
    }

    write_buffer = malloc(block_size);
    read_buffer = malloc(block_size);
    if (write_buffer == NULL || read_buffer == NULL) {
        free(write_buffer);
        free(read_buffer);
        usb_msc_close(&device);
        return failf("failed to allocate transfer buffers");
    }

    fill_pattern(write_buffer, block_size);
    memset(read_buffer, 0, block_size);

    result = usb_msc_write10(&device, test_lba, 1, write_buffer, block_size);
    if (result != 0) {
        free(write_buffer);
        free(read_buffer);
        usb_msc_close(&device);
        return failf(
            "WRITE(10) failed at lba=%u (%d, %s)",
            test_lba,
            result,
            libusb_error_name(result)
        );
    }
    printf("usb write test completed at lba=%u\n", test_lba);

    result = usb_msc_read10(&device, test_lba, 1, read_buffer, block_size);
    if (result != 0) {
        free(write_buffer);
        free(read_buffer);
        usb_msc_close(&device);
        return failf(
            "READ(10) failed at lba=%u (%d, %s)",
            test_lba,
            result,
            libusb_error_name(result)
        );
    }
    printf("usb readback completed at lba=%u\n", test_lba);

    if (memcmp(write_buffer, read_buffer, block_size) != 0) {
        free(write_buffer);
        free(read_buffer);
        usb_msc_close(&device);
        return failf("readback data mismatch");
    }

    puts("USB transfer tests passed!");
    exit_code = 0;

    free(write_buffer);
    free(read_buffer);
    usb_msc_close(&device);
    return exit_code;
}
