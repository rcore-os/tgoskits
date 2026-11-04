#ifndef STARRY_VIRTIO_SOUND_TEST_H
#define STARRY_VIRTIO_SOUND_TEST_H

#include <stddef.h>
#include <stdint.h>
#include <sys/types.h>

#define SOUND_TEST_RATE 48000u
#define SOUND_TEST_CHANNELS 2u

struct sound_test_pcm {
    int16_t *samples;
    size_t frames;
    unsigned int rate;
    unsigned int channels;
};

_Noreturn void sound_test_fail(const char *profile, const char *fmt, ...);

void sound_test_require(int condition, const char *profile, const char *fmt, ...);

int sound_test_spawn(char *const argv[], pid_t *pid);
int sound_test_wait(pid_t pid, unsigned int timeout_ms);
int sound_test_run(char *const argv[], unsigned int timeout_ms);

void sound_test_write_counter_wav(const char *path, size_t frames,
                                  const char *profile);
struct sound_test_pcm sound_test_read_wav(const char *path,
                                          const char *profile);
void sound_test_free_wav(struct sound_test_pcm *pcm);
void sound_test_check_tone(const struct sound_test_pcm *pcm,
                           double expected_hz, const char *profile);

#endif
