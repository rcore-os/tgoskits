#define _GNU_SOURCE

#include "sound_test.h"

#include <errno.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void write_le16(unsigned char *out, unsigned int value)
{
    out[0] = (unsigned char)(value & 0xffu);
    out[1] = (unsigned char)((value >> 8) & 0xffu);
}

static void write_le32(unsigned char *out, unsigned int value)
{
    out[0] = (unsigned char)(value & 0xffu);
    out[1] = (unsigned char)((value >> 8) & 0xffu);
    out[2] = (unsigned char)((value >> 16) & 0xffu);
    out[3] = (unsigned char)((value >> 24) & 0xffu);
}

static unsigned int read_le16(const unsigned char *in)
{
    return (unsigned int)in[0] | ((unsigned int)in[1] << 8);
}

static unsigned int read_le32(const unsigned char *in)
{
    return (unsigned int)in[0] | ((unsigned int)in[1] << 8) |
           ((unsigned int)in[2] << 16) | ((unsigned int)in[3] << 24);
}

void sound_test_write_counter_wav(const char *path, size_t frames,
                                  const char *profile)
{
    unsigned char header[44] = {0};
    size_t data_size;
    FILE *file;

    sound_test_require(frames > 0 && frames <= (1u << 24), profile,
                       "invalid counter WAV frame count %zu", frames);
    data_size = frames * SOUND_TEST_CHANNELS * sizeof(int16_t);
    sound_test_require(data_size <= 0xffffffffu - 36u, profile,
                       "counter WAV is too large");

    memcpy(header, "RIFF", 4);
    write_le32(header + 4, (unsigned int)(36u + data_size));
    memcpy(header + 8, "WAVEfmt ", 8);
    write_le32(header + 16, 16);
    write_le16(header + 20, 1);
    write_le16(header + 22, SOUND_TEST_CHANNELS);
    write_le32(header + 24, SOUND_TEST_RATE);
    write_le32(header + 28, SOUND_TEST_RATE * SOUND_TEST_CHANNELS * 2u);
    write_le16(header + 32, SOUND_TEST_CHANNELS * 2u);
    write_le16(header + 34, 16);
    memcpy(header + 36, "data", 4);
    write_le32(header + 40, (unsigned int)data_size);

    file = fopen(path, "wb");
    sound_test_require(file != NULL, profile, "open %s: %s", path,
                       strerror(errno));
    sound_test_require(fwrite(header, 1, sizeof(header), file) ==
                           sizeof(header),
                       profile, "write WAV header to %s", path);

    for (size_t frame = 0; frame < frames; ++frame) {
        int left = (int)((frame * 97u) % 20001u) - 10000;
        unsigned char sample[4];

        write_le16(sample, (unsigned int)(uint16_t)(int16_t)left);
        write_le16(sample + 2, (unsigned int)(uint16_t)(int16_t)(-left));
        sound_test_require(fwrite(sample, 1, sizeof(sample), file) ==
                               sizeof(sample),
                           profile, "write WAV sample %zu to %s", frame, path);
    }

    sound_test_require(fclose(file) == 0, profile, "close %s: %s", path,
                       strerror(errno));
}

struct sound_test_pcm sound_test_read_wav(const char *path,
                                          const char *profile)
{
    struct sound_test_pcm pcm = {0};
    unsigned char *file_data = NULL;
    size_t file_size;
    size_t offset = 12;
    int have_format = 0;
    FILE *file;

    file = fopen(path, "rb");
    sound_test_require(file != NULL, profile, "open %s: %s", path,
                       strerror(errno));
    sound_test_require(fseek(file, 0, SEEK_END) == 0, profile,
                       "seek %s", path);
    long length = ftell(file);
    sound_test_require(length > 0, profile, "empty WAV %s", path);
    sound_test_require(fseek(file, 0, SEEK_SET) == 0, profile,
                       "rewind %s", path);
    file_size = (size_t)length;
    file_data = malloc(file_size);
    sound_test_require(file_data != NULL, profile, "allocate WAV buffer");
    sound_test_require(fread(file_data, 1, file_size, file) == file_size,
                       profile, "read WAV %s", path);
    sound_test_require(fclose(file) == 0, profile, "close %s: %s", path,
                       strerror(errno));

    sound_test_require(file_size >= 12 && memcmp(file_data, "RIFF", 4) == 0 &&
                           memcmp(file_data + 8, "WAVE", 4) == 0,
                       profile, "not a RIFF/WAVE file: %s", path);

    while (offset + 8 <= file_size) {
        const unsigned char *id = file_data + offset;
        size_t chunk_size = read_le32(file_data + offset + 4);
        size_t start = offset + 8;
        size_t end;

        sound_test_require(chunk_size <= file_size - start, profile,
                           "truncated WAV chunk in %s", path);
        end = start + chunk_size;
        if (memcmp(id, "fmt ", 4) == 0) {
            sound_test_require(!have_format && chunk_size >= 16, profile,
                               "missing or duplicate WAV format in %s", path);
            sound_test_require(read_le16(file_data + start) == 1 &&
                                   read_le16(file_data + start + 2) ==
                                       SOUND_TEST_CHANNELS &&
                                   read_le32(file_data + start + 4) ==
                                       SOUND_TEST_RATE &&
                                   read_le16(file_data + start + 12) == 4 &&
                                   read_le16(file_data + start + 14) == 16,
                               profile,
                               "expected PCM S16LE 48 kHz stereo in %s", path);
            have_format = 1;
        } else if (memcmp(id, "data", 4) == 0) {
            size_t samples;

            sound_test_require(have_format, profile,
                               "WAV data before format in %s", path);
            sound_test_require(chunk_size % 4 == 0, profile,
                               "unaligned WAV data in %s", path);
            pcm.frames = chunk_size / 4;
            pcm.rate = SOUND_TEST_RATE;
            pcm.channels = SOUND_TEST_CHANNELS;
            sound_test_require(pcm.frames > 0, profile,
                               "empty WAV data in %s", path);
            samples = pcm.frames * pcm.channels;
            pcm.samples = malloc(samples * sizeof(*pcm.samples));
            sound_test_require(pcm.samples != NULL, profile,
                               "allocate PCM samples");
            for (size_t index = 0; index < samples; ++index) {
                pcm.samples[index] =
                    (int16_t)(uint16_t)read_le16(
                        file_data + start + index * sizeof(int16_t));
            }
            free(file_data);
            return pcm;
        }

        offset = end + (chunk_size & 1u);
    }

    free(file_data);
    sound_test_fail(profile, "WAV %s contains no data chunk", path);
}

void sound_test_free_wav(struct sound_test_pcm *pcm)
{
    free(pcm->samples);
    pcm->samples = NULL;
    pcm->frames = 0;
}

void sound_test_check_tone(const struct sound_test_pcm *pcm,
                           double expected_hz, const char *profile)
{
    double energy = 0.0;
    double rms;
    double frequency;
    int peak = 0;
    int threshold;
    int below_threshold = 0;
    size_t crossings = 0;
    size_t first_crossing = 0;
    size_t last_crossing = 0;

    sound_test_require(pcm->channels == 2 &&
                           pcm->rate == SOUND_TEST_RATE &&
                           pcm->frames > SOUND_TEST_RATE / 10u,
                       profile, "capture is too short or has wrong format");

    for (size_t frame = 0; frame < pcm->frames; ++frame) {
        int left = pcm->samples[frame * 2];
        int right = pcm->samples[frame * 2 + 1];
        int left_magnitude = left < 0 ? -left : left;
        int right_magnitude = right < 0 ? -right : right;

        if (left_magnitude > peak) {
            peak = left_magnitude;
        }
        if (right_magnitude > peak) {
            peak = right_magnitude;
        }
        energy += (double)left * (double)left;
        energy += (double)right * (double)right;
    }
    rms = sqrt(energy / (double)(pcm->frames * pcm->channels));
    sound_test_require(peak >= 100 && rms >= 20.0, profile,
                       "capture is silent or has implausible amplitude "
                       "(peak=%d rms=%.1f)",
                       peak, rms);

    threshold = peak / 4;
    if (threshold < 1) {
        threshold = 1;
    }
    for (size_t frame = 0; frame < pcm->frames; ++frame) {
        int sample = pcm->samples[frame * 2];

        if (sample <= -threshold) {
            below_threshold = 1;
        } else if (below_threshold && sample >= threshold) {
            if (crossings == 0) {
                first_crossing = frame;
            }
            last_crossing = frame;
            crossings++;
            below_threshold = 0;
        }
    }

    sound_test_require(crossings >= 10 && last_crossing > first_crossing,
                       profile, "capture has no usable tone crossings");
    frequency = (double)(crossings - 1) * (double)pcm->rate /
                (double)(last_crossing - first_crossing);
    sound_test_require(frequency > expected_hz * 0.95 &&
                           frequency < expected_hz * 1.05,
                       profile,
                       "capture tone %.1f Hz is outside %.1f Hz +/-5%%",
                       frequency, expected_hz);

    printf("VIRTIO_SOUND_TONE frames=%zu peak=%d rms=%.1f frequency_hz=%.1f\n",
           pcm->frames, peak, rms, frequency);
    fflush(stdout);
}
