#define _POSIX_C_SOURCE 200809L

#include <libusb.h>

#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/time.h>
#include <unistd.h>

#define USB_AUDIO_CLASS 0x01
#define USB_AUDIO_SUBCLASS_STREAMING 0x02
#define ISO_PACKETS_PER_TRANSFER 64
#define ISO_INFLIGHT_TRANSFERS 4
#define ISO_TRANSFER_TIMEOUT_MS 1000
#define REFERENCE_WAV_PATH "/usr/share/usb-audio-iso/reference.wav"
#define EXPECTED_CHANNELS 2
#define EXPECTED_SAMPLE_RATE 48000
#define EXPECTED_BITS_PER_SAMPLE 16
#define MIN_TOTAL_PACKETS 256

typedef struct wav_file {
    unsigned char *bytes;
    size_t length;
    unsigned char *data;
    size_t data_length;
    uint16_t channels;
    uint32_t sample_rate;
    uint32_t byte_rate;
    uint16_t block_align;
    uint16_t bits_per_sample;
} wav_file_t;

typedef struct audio_candidate {
    libusb_device *device;
    uint8_t config_value;
    uint8_t interface_number;
    uint8_t altsetting;
    uint8_t endpoint_out;
    int max_iso_packet_size;
} audio_candidate_t;

typedef struct transfer_slot {
    struct libusb_transfer *transfer;
    unsigned char *buffer;
    size_t expected_length;
    int packet_count;
    int completed;
} transfer_slot_t;

static int failf(const char *format, ...) {
    va_list args;
    va_start(args, format);
    fputs("USB test failed: ", stdout);
    vprintf(format, args);
    fputc('\n', stdout);
    va_end(args);
    return 1;
}

static uint16_t read_le16(const unsigned char *bytes) {
    return (uint16_t)bytes[0] | ((uint16_t)bytes[1] << 8);
}

static uint32_t read_le32(const unsigned char *bytes) {
    return (uint32_t)bytes[0] | ((uint32_t)bytes[1] << 8) | ((uint32_t)bytes[2] << 16) |
           ((uint32_t)bytes[3] << 24);
}

static const char *transfer_status_name(enum libusb_transfer_status status) {
    switch (status) {
        case LIBUSB_TRANSFER_COMPLETED:
            return "COMPLETED";
        case LIBUSB_TRANSFER_ERROR:
            return "ERROR";
        case LIBUSB_TRANSFER_TIMED_OUT:
            return "TIMED_OUT";
        case LIBUSB_TRANSFER_CANCELLED:
            return "CANCELLED";
        case LIBUSB_TRANSFER_STALL:
            return "STALL";
        case LIBUSB_TRANSFER_NO_DEVICE:
            return "NO_DEVICE";
        case LIBUSB_TRANSFER_OVERFLOW:
            return "OVERFLOW";
        default:
            return "UNKNOWN";
    }
}

static void free_transfer_slot(transfer_slot_t *slot) {
    if (slot->transfer != NULL) {
        libusb_free_transfer(slot->transfer);
        slot->transfer = NULL;
    }
    free(slot->buffer);
    slot->buffer = NULL;
    slot->expected_length = 0;
    slot->packet_count = 0;
    slot->completed = 0;
}

static int load_wav_file(const char *path, wav_file_t *wav) {
    memset(wav, 0, sizeof(*wav));

    FILE *file = fopen(path, "rb");
    if (file == NULL) {
        return failf("failed to open reference wav `%s`", path);
    }

    if (fseek(file, 0, SEEK_END) != 0) {
        fclose(file);
        return failf("failed to seek reference wav `%s`", path);
    }
    long file_size = ftell(file);
    if (file_size < 0) {
        fclose(file);
        return failf("failed to determine size of `%s`", path);
    }
    if (fseek(file, 0, SEEK_SET) != 0) {
        fclose(file);
        return failf("failed to rewind reference wav `%s`", path);
    }

    wav->length = (size_t)file_size;
    wav->bytes = malloc(wav->length);
    if (wav->bytes == NULL) {
        fclose(file);
        return failf("failed to allocate %zu bytes for reference wav", wav->length);
    }
    if (fread(wav->bytes, 1, wav->length, file) != wav->length) {
        fclose(file);
        free(wav->bytes);
        wav->bytes = NULL;
        return failf("failed to read `%s`", path);
    }
    fclose(file);

    if (wav->length < 12 || memcmp(wav->bytes, "RIFF", 4) != 0 ||
        memcmp(wav->bytes + 8, "WAVE", 4) != 0) {
        return failf("reference wav `%s` is not a RIFF/WAVE file", path);
    }

    size_t cursor = 12;
    bool found_fmt = false;
    bool found_data = false;
    while (cursor + 8 <= wav->length) {
        const unsigned char *chunk = wav->bytes + cursor;
        const uint32_t chunk_length = read_le32(chunk + 4);
        cursor += 8;
        if (cursor + chunk_length > wav->length) {
            return failf("reference wav `%s` has a truncated chunk", path);
        }

        if (memcmp(chunk, "fmt ", 4) == 0) {
            if (chunk_length < 16) {
                return failf("reference wav `%s` has an invalid fmt chunk", path);
            }
            const unsigned char *fmt = wav->bytes + cursor;
            const uint16_t audio_format = read_le16(fmt);
            if (audio_format != 1) {
                return failf("reference wav `%s` is not PCM", path);
            }
            wav->channels = read_le16(fmt + 2);
            wav->sample_rate = read_le32(fmt + 4);
            wav->byte_rate = read_le32(fmt + 8);
            wav->block_align = read_le16(fmt + 12);
            wav->bits_per_sample = read_le16(fmt + 14);
            found_fmt = true;
        } else if (memcmp(chunk, "data", 4) == 0) {
            wav->data = wav->bytes + cursor;
            wav->data_length = chunk_length;
            found_data = true;
        }

        cursor += chunk_length;
        if ((chunk_length & 1u) != 0) {
            cursor += 1;
        }
    }

    if (!found_fmt || !found_data) {
        return failf("reference wav `%s` is missing fmt/data chunks", path);
    }
    if (wav->channels != EXPECTED_CHANNELS || wav->sample_rate != EXPECTED_SAMPLE_RATE ||
        wav->bits_per_sample != EXPECTED_BITS_PER_SAMPLE) {
        return failf(
            "reference wav format mismatch: channels=%u rate=%u bits=%u",
            wav->channels,
            wav->sample_rate,
            wav->bits_per_sample
        );
    }
    if (wav->data_length == 0 || wav->data_length % wav->block_align != 0) {
        return failf("reference wav payload length %zu is invalid", wav->data_length);
    }
    return 0;
}

static void free_wav_file(wav_file_t *wav) {
    free(wav->bytes);
    memset(wav, 0, sizeof(*wav));
}

static bool is_audio_streaming_iso_out(
    const struct libusb_interface_descriptor *if_desc,
    uint8_t *endpoint_out,
    int *max_packet_size,
    libusb_device *device
) {
    if (if_desc->bInterfaceClass != USB_AUDIO_CLASS ||
        if_desc->bInterfaceSubClass != USB_AUDIO_SUBCLASS_STREAMING) {
        return false;
    }

    for (int ep_index = 0; ep_index < if_desc->bNumEndpoints; ep_index++) {
        const struct libusb_endpoint_descriptor *endpoint = &if_desc->endpoint[ep_index];
        if ((endpoint->bmAttributes & LIBUSB_TRANSFER_TYPE_MASK) !=
            LIBUSB_TRANSFER_TYPE_ISOCHRONOUS) {
            continue;
        }
        if ((endpoint->bEndpointAddress & LIBUSB_ENDPOINT_DIR_MASK) != LIBUSB_ENDPOINT_OUT) {
            continue;
        }

        int packet_size = libusb_get_max_iso_packet_size(device, endpoint->bEndpointAddress);
        if (packet_size <= 0) {
            continue;
        }
        *endpoint_out = endpoint->bEndpointAddress;
        *max_packet_size = packet_size;
        return true;
    }
    return false;
}

static int find_audio_output_candidate(
    libusb_device **device_list,
    ssize_t count,
    audio_candidate_t *candidate
) {
    memset(candidate, 0, sizeof(*candidate));
    int best_packet_size = -1;

    for (ssize_t dev_index = 0; dev_index < count; dev_index++) {
        libusb_device *device = device_list[dev_index];
        struct libusb_device_descriptor descriptor;
        int result = libusb_get_device_descriptor(device, &descriptor);
        if (result != 0) {
            continue;
        }

        printf(
            "usb audio candidate: bus=%u addr=%u vid=%04x pid=%04x configs=%u\n",
            libusb_get_bus_number(device),
            libusb_get_device_address(device),
            descriptor.idVendor,
            descriptor.idProduct,
            descriptor.bNumConfigurations
        );
        fflush(stdout);

        for (uint8_t config_index = 0; config_index < descriptor.bNumConfigurations;
             config_index++) {
            struct libusb_config_descriptor *config = NULL;
            result = libusb_get_config_descriptor(device, config_index, &config);
            if (result != 0 || config == NULL) {
                continue;
            }

            for (int interface_index = 0; interface_index < config->bNumInterfaces;
                 interface_index++) {
                const struct libusb_interface *interface = &config->interface[interface_index];
                for (int alt_index = 0; alt_index < interface->num_altsetting; alt_index++) {
                    const struct libusb_interface_descriptor *if_desc =
                        &interface->altsetting[alt_index];
                    uint8_t endpoint_out = 0;
                    int packet_size = 0;

                    if (!is_audio_streaming_iso_out(
                            if_desc,
                            &endpoint_out,
                            &packet_size,
                            device
                        )) {
                        continue;
                    }

                    printf(
                        "usb audio streaming altsetting: if=%u alt=%u ep=%02x packet=%d\n",
                        if_desc->bInterfaceNumber,
                        if_desc->bAlternateSetting,
                        endpoint_out,
                        packet_size
                    );
                    fflush(stdout);

                    if (packet_size > best_packet_size) {
                        best_packet_size = packet_size;
                        candidate->device = device;
                        candidate->config_value = config->bConfigurationValue;
                        candidate->interface_number = if_desc->bInterfaceNumber;
                        candidate->altsetting = if_desc->bAlternateSetting;
                        candidate->endpoint_out = endpoint_out;
                        candidate->max_iso_packet_size = packet_size;
                    }
                }
            }

            libusb_free_config_descriptor(config);
        }
    }

    if (best_packet_size <= 0 || candidate->device == NULL) {
        return failf("no USB Audio streaming altsetting with isochronous OUT endpoint found");
    }
    return 0;
}

static void LIBUSB_CALL iso_transfer_callback(struct libusb_transfer *transfer) {
    transfer_slot_t *slot = transfer->user_data;
    if (slot != NULL) {
        slot->completed = 1;
    }
}

static int submit_next_iso_transfer(
    libusb_device_handle *handle,
    uint8_t endpoint_out,
    const unsigned char *data,
    size_t data_length,
    size_t *next_offset,
    int packet_size,
    transfer_slot_t *slot
) {
    const size_t remaining = data_length - *next_offset;
    const size_t transfer_capacity = (size_t)packet_size * ISO_PACKETS_PER_TRANSFER;
    const size_t transfer_length = remaining < transfer_capacity ? remaining : transfer_capacity;
    int packet_count = (int)((transfer_length + (size_t)packet_size - 1) / (size_t)packet_size);
    if (packet_count <= 0) {
        return 0;
    }

    slot->buffer = malloc(transfer_length);
    if (slot->buffer == NULL) {
        return failf("failed to allocate %zu-byte iso transfer buffer", transfer_length);
    }
    memcpy(slot->buffer, data + *next_offset, transfer_length);
    slot->transfer = libusb_alloc_transfer(packet_count);
    if (slot->transfer == NULL) {
        free(slot->buffer);
        slot->buffer = NULL;
        return failf("libusb_alloc_transfer(%d) failed", packet_count);
    }

    libusb_fill_iso_transfer(
        slot->transfer,
        handle,
        endpoint_out,
        slot->buffer,
        (int)transfer_length,
        packet_count,
        iso_transfer_callback,
        slot,
        ISO_TRANSFER_TIMEOUT_MS
    );

    size_t cursor = 0;
    for (int packet_index = 0; packet_index < packet_count; packet_index++) {
        const size_t packet_length =
            (transfer_length - cursor) < (size_t)packet_size ? (transfer_length - cursor)
                                                             : (size_t)packet_size;
        slot->transfer->iso_packet_desc[packet_index].length = (unsigned int)packet_length;
        cursor += packet_length;
    }

    int result = libusb_submit_transfer(slot->transfer);
    if (result != 0) {
        libusb_free_transfer(slot->transfer);
        slot->transfer = NULL;
        free(slot->buffer);
        slot->buffer = NULL;
        return failf(
            "libusb_submit_transfer failed (%d, %s)",
            result,
            libusb_error_name(result)
        );
    }

    slot->expected_length = transfer_length;
    slot->packet_count = packet_count;
    slot->completed = 0;
    *next_offset += transfer_length;
    return 0;
}

static int finalize_iso_transfer(
    transfer_slot_t *slot,
    size_t *completed_bytes,
    size_t *completed_packets,
    bool *saw_nonzero_packet
) {
    struct libusb_transfer *transfer = slot->transfer;
    if (transfer == NULL) {
        return failf("iso transfer slot completed without a transfer");
    }
    if (transfer->status != LIBUSB_TRANSFER_COMPLETED) {
        return failf("iso transfer status %s", transfer_status_name(transfer->status));
    }

    size_t actual_total = 0;
    for (int packet_index = 0; packet_index < transfer->num_iso_packets; packet_index++) {
        const struct libusb_iso_packet_descriptor *packet = &transfer->iso_packet_desc[packet_index];
        if (packet->status != LIBUSB_TRANSFER_COMPLETED) {
            return failf(
                "iso packet %d status %s",
                packet_index,
                transfer_status_name(packet->status)
            );
        }
        if (packet->actual_length != packet->length) {
            return failf(
                "iso packet %d actual_length=%u expected=%u",
                packet_index,
                packet->actual_length,
                packet->length
            );
        }
        actual_total += packet->actual_length;
        if (packet->actual_length > 0) {
            *saw_nonzero_packet = true;
        }
    }
    if (actual_total != slot->expected_length) {
        return failf(
            "iso transfer packet_total=%zu expected=%zu transfer_actual=%d",
            actual_total,
            slot->expected_length,
            transfer->actual_length
        );
    }

    *completed_bytes += actual_total;
    *completed_packets += (size_t)transfer->num_iso_packets;
    free_transfer_slot(slot);
    return 0;
}

static int run_iso_playback(
    libusb_context *ctx,
    libusb_device_handle *handle,
    uint8_t endpoint_out,
    const wav_file_t *wav,
    int packet_size,
    size_t *total_packets_out
) {
    transfer_slot_t slots[ISO_INFLIGHT_TRANSFERS];
    memset(slots, 0, sizeof(slots));

    const size_t required_packets =
        (wav->data_length + (size_t)packet_size - 1) / (size_t)packet_size;
    if (required_packets < MIN_TOTAL_PACKETS) {
        return failf(
            "reference wav is too small for iso validation: packets=%zu min=%d",
            required_packets,
            MIN_TOTAL_PACKETS
        );
    }

    size_t next_offset = 0;
    size_t completed_bytes = 0;
    size_t completed_packets = 0;
    int inflight = 0;
    bool saw_nonzero_packet = false;

    while (next_offset < wav->data_length || inflight > 0) {
        while (next_offset < wav->data_length && inflight < ISO_INFLIGHT_TRANSFERS) {
            transfer_slot_t *free_slot = NULL;
            for (int slot_index = 0; slot_index < ISO_INFLIGHT_TRANSFERS; slot_index++) {
                if (slots[slot_index].transfer == NULL) {
                    free_slot = &slots[slot_index];
                    break;
                }
            }
            if (free_slot == NULL) {
                break;
            }
            int result = submit_next_iso_transfer(
                handle,
                endpoint_out,
                wav->data,
                wav->data_length,
                &next_offset,
                packet_size,
                free_slot
            );
            if (result != 0) {
                for (int slot_index = 0; slot_index < ISO_INFLIGHT_TRANSFERS; slot_index++) {
                    free_transfer_slot(&slots[slot_index]);
                }
                return result;
            }
            inflight++;
        }

        struct timeval timeout = {
            .tv_sec = 0,
            .tv_usec = 100000,
        };
        int result = libusb_handle_events_timeout_completed(ctx, &timeout, NULL);
        if (result != 0) {
            for (int slot_index = 0; slot_index < ISO_INFLIGHT_TRANSFERS; slot_index++) {
                free_transfer_slot(&slots[slot_index]);
            }
            return failf(
                "libusb_handle_events_timeout_completed failed (%d, %s)",
                result,
                libusb_error_name(result)
            );
        }

        for (int slot_index = 0; slot_index < ISO_INFLIGHT_TRANSFERS; slot_index++) {
            if (slots[slot_index].transfer != NULL && slots[slot_index].completed) {
                result = finalize_iso_transfer(
                    &slots[slot_index],
                    &completed_bytes,
                    &completed_packets,
                    &saw_nonzero_packet
                );
                if (result != 0) {
                    for (int cleanup_index = 0; cleanup_index < ISO_INFLIGHT_TRANSFERS;
                         cleanup_index++) {
                        free_transfer_slot(&slots[cleanup_index]);
                    }
                    return result;
                }
                inflight--;
            }
        }
    }

    if (!saw_nonzero_packet) {
        return failf("iso playback completed without any non-zero packet payload");
    }
    if (completed_bytes != wav->data_length) {
        return failf(
            "iso playback completed_bytes=%zu expected=%zu",
            completed_bytes,
            wav->data_length
        );
    }
    if (completed_packets != required_packets) {
        return failf(
            "iso playback completed_packets=%zu expected=%zu",
            completed_packets,
            required_packets
        );
    }

    *total_packets_out = completed_packets;
    return 0;
}

int main(void) {
    libusb_context *ctx = NULL;
    libusb_device **device_list = NULL;
    libusb_device_handle *handle = NULL;
    wav_file_t wav;
    audio_candidate_t candidate;
    int exit_code = 1;
    int interface_claimed = 0;

    memset(&wav, 0, sizeof(wav));
    memset(&candidate, 0, sizeof(candidate));

    int result = load_wav_file(REFERENCE_WAV_PATH, &wav);
    if (result != 0) {
        return 1;
    }

    result = libusb_init(&ctx);
    if (result != 0) {
        free_wav_file(&wav);
        return failf("libusb_init failed (%d, %s)", result, libusb_error_name(result));
    }

    ssize_t count = libusb_get_device_list(ctx, &device_list);
    if (count < 0) {
        libusb_exit(ctx);
        free_wav_file(&wav);
        return failf(
            "libusb_get_device_list failed (%zd, %s)",
            count,
            libusb_error_name((int)count)
        );
    }

    result = find_audio_output_candidate(device_list, count, &candidate);
    if (result != 0) {
        goto cleanup;
    }

    result = libusb_open(candidate.device, &handle);
    if (result != 0 || handle == NULL) {
        result = failf("libusb_open failed (%d, %s)", result, libusb_error_name(result));
        goto cleanup;
    }

    int active_config = 0;
    result = libusb_get_configuration(handle, &active_config);
    if (result != 0) {
        result = failf(
            "libusb_get_configuration failed (%d, %s)",
            result,
            libusb_error_name(result)
        );
        goto cleanup;
    }
    if (active_config != candidate.config_value && candidate.config_value != 0) {
        result = libusb_set_configuration(handle, candidate.config_value);
        if (result != 0) {
            result = failf(
                "libusb_set_configuration(%u) failed (%d, %s)",
                candidate.config_value,
                result,
                libusb_error_name(result)
            );
            goto cleanup;
        }
    }

    result = libusb_claim_interface(handle, candidate.interface_number);
    if (result != 0) {
        result = failf(
            "libusb_claim_interface(%u) failed (%d, %s)",
            candidate.interface_number,
            result,
            libusb_error_name(result)
        );
        goto cleanup;
    }
    interface_claimed = 1;

    result = libusb_set_interface_alt_setting(
        handle,
        candidate.interface_number,
        candidate.altsetting
    );
    if (result != 0) {
        result = failf(
            "libusb_set_interface_alt_setting(if=%u, alt=%u) failed (%d, %s)",
            candidate.interface_number,
            candidate.altsetting,
            result,
            libusb_error_name(result)
        );
        goto cleanup;
    }

    size_t total_packets = 0;
    printf(
        "usb audio iso selection: if=%u alt=%u ep=%02x packet=%d bytes=%zu\n",
        candidate.interface_number,
        candidate.altsetting,
        candidate.endpoint_out,
        candidate.max_iso_packet_size,
        wav.data_length
    );
    fflush(stdout);

    result = run_iso_playback(
        ctx,
        handle,
        candidate.endpoint_out,
        &wav,
        candidate.max_iso_packet_size,
        &total_packets
    );
    if (result != 0) {
        goto cleanup;
    }

    printf(
        "usb audio iso playback ok: packets=%zu bytes=%zu block_align=%u byte_rate=%u\n",
        total_packets,
        wav.data_length,
        wav.block_align,
        wav.byte_rate
    );
    fflush(stdout);

    unsigned drain_seconds =
        (unsigned)((wav.data_length + wav.byte_rate - 1) / wav.byte_rate) + 1u;
    sleep(drain_seconds);
    puts("USB audio iso tests passed!");
    exit_code = 0;

cleanup:
    if (interface_claimed) {
        (void)libusb_set_interface_alt_setting(handle, candidate.interface_number, 0);
        (void)libusb_release_interface(handle, candidate.interface_number);
    }
    if (handle != NULL) {
        libusb_close(handle);
    }
    if (device_list != NULL) {
        libusb_free_device_list(device_list, 1);
    }
    if (ctx != NULL) {
        libusb_exit(ctx);
    }
    free_wav_file(&wav);
    return exit_code;
}
