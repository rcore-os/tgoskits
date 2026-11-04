#define _GNU_SOURCE

#include "sound_test.h"

#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#define PROFILE "CAPTURE"
#define CAPTURE_PATH "/tmp/virtio-sound-capture.wav"
#define DUPLEX_PATH "/tmp/virtio-sound-duplex.wav"
#define RECOVERY_PATH "/tmp/virtio-sound-recovery.wav"
#define TONE_PATH "/tmp/virtio-sound-duplex-tone.wav"

static void require_duration(const struct sound_test_pcm *pcm,
                             unsigned int seconds)
{
    size_t expected = (size_t)SOUND_TEST_RATE * seconds;
    size_t tolerance = SOUND_TEST_RATE / 10u;

    sound_test_require(pcm->frames + tolerance >= expected &&
                           pcm->frames <= expected + tolerance,
                       PROFILE,
                       "captured %zu frames for %u s, expected %zu +/- %zu",
                       pcm->frames, seconds, expected, tolerance);
}

static void check_capture_file(const char *path, unsigned int seconds)
{
    struct sound_test_pcm pcm = sound_test_read_wav(path, PROFILE);

    require_duration(&pcm, seconds);
    sound_test_check_tone(&pcm, 997.0, PROFILE);
    sound_test_free_wav(&pcm);
}

static int run_arecord(const char *path, const char *seconds)
{
    char *arecord[] = {
        "arecord",
        "-D",
        "hw:0,0",
        "-f",
        "S16_LE",
        "-r",
        "48000",
        "-c",
        "2",
        "-t",
        "wav",
        "-d",
        (char *)seconds,
        (char *)path,
        NULL,
    };

    return sound_test_run(arecord, 120000);
}

static void run_duplex(void)
{
    pid_t playback;
    pid_t capture;

    sound_test_write_counter_wav(TONE_PATH, SOUND_TEST_RATE, PROFILE);
    char *aplay[] = {
        "aplay",
        "-q",
        "-D",
        "hw:0,0",
        TONE_PATH,
        NULL,
    };
    char *arecord[] = {
        "arecord",
        "-D",
        "hw:0,0",
        "-f",
        "S16_LE",
        "-r",
        "48000",
        "-c",
        "2",
        "-t",
        "wav",
        "-d",
        "2",
        DUPLEX_PATH,
        NULL,
    };

    sound_test_require(sound_test_spawn(aplay, &playback) == 0, PROFILE,
                       "spawn duplex aplay");
    if (sound_test_spawn(arecord, &capture) != 0) {
        kill(playback, SIGKILL);
        (void)sound_test_wait(playback, 1000);
        sound_test_fail(PROFILE, "spawn duplex arecord");
    }

    int playback_status = sound_test_wait(playback, 120000);
    int capture_status = sound_test_wait(capture, 120000);
    sound_test_require(playback_status == 0, PROFILE,
                       "duplex aplay exited with status %d", playback_status);
    sound_test_require(capture_status == 0, PROFILE,
                       "duplex arecord exited with status %d", capture_status);

    check_capture_file(DUPLEX_PATH, 2);
}

int main(void)
{
    int status = run_arecord(CAPTURE_PATH, "2");

    sound_test_require(status == 0, PROFILE,
                       "arecord -d 2 exited with status %d", status);
    check_capture_file(CAPTURE_PATH, 2);

    run_duplex();

    status = run_arecord(RECOVERY_PATH, "1");
    sound_test_require(status == 0, PROFILE,
                       "post-duplex recovery arecord exited with status %d",
                       status);
    check_capture_file(RECOVERY_PATH, 1);

    puts("VIRTIO_SOUND_CAPTURE_PASSED");
    fflush(stdout);
    return 0;
}
