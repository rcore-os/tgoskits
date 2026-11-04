#define _GNU_SOURCE

#include "sound_test.h"

#include <errno.h>
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <unistd.h>

#define PROFILE "PLAYBACK"
#define WAV_PATH "/tmp/virtio-sound-playback.wav"
#define PLAYBACK_COUNTER_FRAMES 4096u
/*
 * The runner kills QEMU immediately after the guest marker, before QEMU's
 * stdio-backed WAV writer necessarily flushes.  A finite zero tail keeps the
 * writer active after the counter payload so those frames reach the host file.
 */
#define PLAYBACK_SILENCE_FRAMES 4096u

static void write_u32_le(FILE *file, uint32_t value, const char *what)
{
    unsigned char bytes[4] = {
        (unsigned char)(value & 0xffu),
        (unsigned char)((value >> 8) & 0xffu),
        (unsigned char)((value >> 16) & 0xffu),
        (unsigned char)((value >> 24) & 0xffu),
    };

    sound_test_require(fwrite(bytes, 1, sizeof(bytes), file) == sizeof(bytes),
                       PROFILE, "write %s: %s", what, strerror(errno));
}

static void append_silence_tail(const char *path)
{
    const uint32_t silence_bytes =
        (uint32_t)(PLAYBACK_SILENCE_FRAMES * SOUND_TEST_CHANNELS *
                   sizeof(int16_t));
    const uint32_t data_size =
        (uint32_t)((PLAYBACK_COUNTER_FRAMES + PLAYBACK_SILENCE_FRAMES) *
                   SOUND_TEST_CHANNELS * sizeof(int16_t));
    static const unsigned char zeros[4096] = {0};
    unsigned char header[44];
    size_t remaining = silence_bytes;
    FILE *file = fopen(path, "r+b");

    sound_test_require(file != NULL, PROFILE, "open %s: %s", path,
                       strerror(errno));
    sound_test_require(fread(header, 1, sizeof(header), file) ==
                           sizeof(header),
                       PROFILE, "read %s header: %s", path, strerror(errno));
    sound_test_require(memcmp(header, "RIFF", 4) == 0 &&
                           memcmp(header + 8, "WAVE", 4) == 0 &&
                           memcmp(header + 36, "data", 4) == 0,
                       PROFILE, "unexpected generated WAV header in %s",
                       path);
    sound_test_require(fseek(file, 4, SEEK_SET) == 0, PROFILE,
                       "seek %s RIFF size", path);
    write_u32_le(file, 36u + data_size, "RIFF size");
    sound_test_require(fseek(file, 40, SEEK_SET) == 0, PROFILE,
                       "seek %s data size", path);
    write_u32_le(file, data_size, "data size");
    sound_test_require(fseek(file, 0, SEEK_END) == 0, PROFILE,
                       "seek %s end", path);

    while (remaining > 0) {
        size_t chunk = remaining < sizeof(zeros) ? remaining : sizeof(zeros);

        sound_test_require(fwrite(zeros, 1, chunk, file) == chunk, PROFILE,
                           "append silence tail to %s: %s", path,
                           strerror(errno));
        remaining -= chunk;
    }
    sound_test_require(fclose(file) == 0, PROFILE, "close %s: %s", path,
                       strerror(errno));
}

int main(void)
{
    sound_test_write_counter_wav(WAV_PATH, PLAYBACK_COUNTER_FRAMES, PROFILE);
    append_silence_tail(WAV_PATH);

    char *aplay[] = {
        "aplay",
        "-q",
        "-D",
        "hw:0,0",
        WAV_PATH,
        NULL,
    };
    int status = sound_test_run(aplay, 120000);

    sound_test_require(status == 0, PROFILE,
                       "aplay -D hw:0,0 exited with status %d", status);
    sound_test_require(unlink(WAV_PATH) == 0 ||
                           access(WAV_PATH, F_OK) != 0,
                       PROFILE, "remove %s", WAV_PATH);

    puts("VIRTIO_SOUND_PLAYBACK_PASSED");
    fflush(stdout);
    return 0;
}
