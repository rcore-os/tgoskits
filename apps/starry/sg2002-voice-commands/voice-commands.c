// SPDX-License-Identifier: Apache-2.0
#define _POSIX_C_SOURCE 200809L
#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <sherpa-onnx/c-api/c-api.h>

enum { RATE = 16000, CHUNK = 320, TAIL = 12800 };
static volatile sig_atomic_t interrupted;
static const char *const commands[] = {"forward", "backward", "left", "right", "stop"};

struct recognizer {
    const SherpaOnnxKeywordSpotter *spotter;
    const SherpaOnnxOnlineStream *stream;
    uint64_t samples;
};

static void on_signal(int number)
{
    interrupted = number;
}

static int decode(struct recognizer *r)
{
    while (!interrupted && SherpaOnnxIsKeywordStreamReady(r->spotter, r->stream)) {
        SherpaOnnxDecodeKeywordStream(r->spotter, r->stream);
        const SherpaOnnxKeywordResult *result = SherpaOnnxGetKeywordResult(r->spotter, r->stream);
        if (!result) {
            fprintf(stderr, "cannot obtain keyword result\n");
            return -1;
        }
        int command = -1;
        int detected = result->keyword && result->keyword[0];
        if (detected) {
            for (int i = 0; i < 5; ++i)
                if (strcmp(result->keyword, commands[i]) == 0)
                    command = i;
            // Consume each detection before producing an event, including rejected labels.
            SherpaOnnxResetKeywordStream(r->spotter, r->stream);
        }
        SherpaOnnxDestroyKeywordResult(result);
        if (interrupted)
            return -1;
        // Time is the consumed audio position, including EOF silence, not wall time.
        double time = (double)r->samples / RATE;
        if (command < 0)
            continue;
        if (printf("{\"command\":\"%s\",\"time\":%.3f}\n", commands[command], time) < 0 ||
            fflush(stdout) == EOF) {
            fprintf(stderr, "cannot write command output: %s\n", strerror(errno));
            return -1;
        }
    }
    return interrupted ? -1 : 0;
}

static int feed(struct recognizer *r, const float *samples, int count)
{
    if (interrupted)
        return -1;
    SherpaOnnxOnlineStreamAcceptWaveform(r->stream, RATE, samples, count);
    r->samples += (unsigned)count;
    return decode(r);
}

static int read_raw(struct recognizer *r, FILE *input)
{
    unsigned char bytes[CHUNK * 2];
    size_t used = 0;
    for (;;) {
        if (interrupted)
            return -1;
        errno = 0;
        size_t n = fread(bytes + used, 1, sizeof(bytes) - used, input);
        int error = errno;
        used += n;
        if (ferror(input)) {
            if (error != EINTR) {
                fprintf(stderr, "cannot read PCM input: %s\n", error ? strerror(error) : "I/O error");
                return -1;
            }
            clearerr(input);
        }
        int eof = feof(input);
        if (eof && used % 2) {
            fprintf(stderr, "truncated S16_LE input: odd byte count\n");
            return -1;
        }
        if (used == sizeof(bytes) || (eof && used)) {
            float samples[CHUNK];
            for (size_t i = 0; i < used / 2; ++i) {
                int value = bytes[2 * i] | ((unsigned)bytes[2 * i + 1] << 8);
                if (value >= 32768)
                    value -= 65536;
                samples[i] = (float)value / 32768.0f;
            }
            if (feed(r, samples, (int)(used / 2)) != 0)
                return -1;
            used = 0;
        }
        if (eof)
            return 0;
    }
}

static uint32_t little_endian(const unsigned char *bytes, int count)
{
    uint32_t value = 0;
    for (int i = 0; i < count; ++i)
        value |= (uint32_t)bytes[i] << (8 * i);
    return value;
}

static int wave_bytes(FILE *input, unsigned char *bytes, size_t count)
{
    while (count && !interrupted) {
        errno = 0;
        size_t n = fread(bytes, 1, count, input);
        bytes += n;
        count -= n;
        if (ferror(input)) {
            if (errno != EINTR) {
                fprintf(stderr, "cannot read WAV input\n");
                return -1;
            }
            clearerr(input);
        }
        if (feof(input)) {
            fprintf(stderr, "truncated WAV input\n");
            return -1;
        }
    }
    return interrupted ? -1 : 0;
}

static int read_wave(struct recognizer *r, const char *path)
{
    FILE *input = fopen(path, "rb");
    if (!input) {
        perror("open WAV input");
        return -1;
    }
    unsigned char bytes[CHUNK * 2];
    int status = -1, format_seen = 0, data_seen = 0;
    if (wave_bytes(input, bytes, 12))
        goto done;
    uint32_t size = little_endian(bytes + 4, 4);
    if (memcmp(bytes, "RIFF", 4) || memcmp(bytes + 8, "WAVE", 4) || size < 4)
        goto invalid;
    uint32_t remaining = size - 4;
    while (remaining) {
        if (remaining < 8)
            goto invalid;
        if (wave_bytes(input, bytes, 8))
            goto done;
        remaining -= 8;
        uint32_t count = little_endian(bytes + 4, 4);
        uint32_t padding = count & 1;
        if (count > remaining || padding > remaining - count)
            goto invalid;
        remaining -= count + padding;
        int is_format = !memcmp(bytes, "fmt ", 4);
        int is_data = !memcmp(bytes, "data", 4);
        if (is_format) {
            if (format_seen || data_seen || count < 16)
                goto invalid;
            if (wave_bytes(input, bytes, 16))
                goto done;
            if (little_endian(bytes, 2) != 1 || little_endian(bytes + 2, 2) != 1 ||
                little_endian(bytes + 4, 4) != RATE ||
                little_endian(bytes + 8, 4) != RATE * 2 ||
                little_endian(bytes + 12, 2) != 2 || little_endian(bytes + 14, 2) != 16)
                goto invalid;
            format_seen = 1;
            count -= 16;
        } else if (is_data) {
            if (!format_seen || data_seen || !count || count % 2)
                goto invalid;
            data_seen = 1;
        }
        // Consume each chunk once; only data samples enter the recognizer.
        while (count) {
            size_t n = count < sizeof(bytes) ? count : sizeof(bytes);
            if (wave_bytes(input, bytes, n))
                goto done;
            if (is_data) {
                float samples[CHUNK];
                for (size_t i = 0; i < n / 2; ++i) {
                    int value = (int)little_endian(bytes + 2 * i, 2);
                    if (value >= 32768)
                        value -= 65536;
                    samples[i] = (float)value / 32768.0f;
                }
                if (feed(r, samples, (int)(n / 2)))
                    goto done;
            }
            count -= (uint32_t)n;
        }
        if (padding && wave_bytes(input, bytes, 1))
            goto done;
    }
    if (!data_seen)
        goto invalid;
    // Reject bytes outside the declared RIFF container rather than ignoring them.
    if (fgetc(input) != EOF)
        goto invalid;
    if (ferror(input) || interrupted)
        goto done;
    status = 0;
    goto done;
invalid:
    fprintf(stderr, "WAV must be a complete nonempty 16000 Hz mono S16_LE PCM file\n");
done:
    fclose(input);
    return status;
}

static void usage(const char *program)
{
    fprintf(stderr, "Usage: %s MODEL_DIR [--raw-stdin | --raw-file PCM | WAV]\n"
            "Recognize Chinese commands; write command/time JSONL to stdout.\n"
            "Default input: raw stdin, S16_LE mono 16000 Hz. WAV must be S16_LE mono 16000 Hz.\n"
            "Time is consumed audio seconds (including 0.8 s EOF padding).\n"
            "Keywords: 小车前进/小车后退/小车左转/小车右转/停止; labels:\n"
            "forward/backward/left/right/stop. No motor control is performed.\n", program);
}

int main(int argc, char **argv)
{
    if (argc == 2 && strcmp(argv[1], "--help") == 0) {
        usage(argv[0]);
        return 0;
    }
    if (argc < 2 || argc > 4 || argv[1][0] == '-' ||
        (argc == 4 && strcmp(argv[2], "--raw-file") != 0) ||
        (argc == 3 && argv[2][0] == '-' && strcmp(argv[2], "--raw-stdin") != 0)) {
        usage(argv[0]);
        return 2;
    }
    struct sigaction action = {0};
    action.sa_handler = on_signal;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGINT, &action, NULL) || sigaction(SIGTERM, &action, NULL)) {
        perror("install signal handler");
        return 1;
    }
    action.sa_handler = SIG_IGN;
    if (sigaction(SIGPIPE, &action, NULL)) {
        perror("ignore SIGPIPE");
        return 1;
    }

    const char *const names[] = {
        "encoder.onnx", "decoder.onnx", "joiner.onnx", "tokens.txt", "keywords.txt"
    };
    char *paths[5] = {0};
    struct recognizer r = {0};
    FILE *input = stdin;
    int status = 1;
    for (int i = 0; i < 5; ++i) {
        size_t length = strlen(argv[1]) + strlen(names[i]) + 2;
        paths[i] = malloc(length);
        if (!paths[i]) {
            perror("allocate model path");
            goto cleanup;
        }
        snprintf(paths[i], length, "%s/%s", argv[1], names[i]);
        if (!SherpaOnnxFileExists(paths[i])) {
            fprintf(stderr, "missing model asset: %s\n", paths[i]);
            goto cleanup;
        }
    }
    SherpaOnnxKeywordSpotterConfig config = {0};
    config.feat_config.sample_rate = RATE;
    config.feat_config.feature_dim = 80;
    config.model_config.transducer.encoder = paths[0];
    config.model_config.transducer.decoder = paths[1];
    config.model_config.transducer.joiner = paths[2];
    config.model_config.tokens = paths[3];
    config.model_config.num_threads = 1;
    config.model_config.provider = "cpu";
    // The pinned encoder is Zipformer2; avoid loading a session just to detect it.
    config.model_config.model_type = "zipformer2";
    // Preserve phonetic alternatives through the shared Chinese command prefix.
    config.max_active_paths = 16;
    config.num_trailing_blanks = 1;
    config.keywords_score = 1.0f;
    config.keywords_threshold = 0.20f;
    config.keywords_file = paths[4];
    r.spotter = SherpaOnnxCreateKeywordSpotter(&config);
    if (!r.spotter) {
        fprintf(stderr, "cannot create keyword spotter\n");
        goto cleanup;
    }
    r.stream = SherpaOnnxCreateKeywordStream(r.spotter);
    if (!r.stream) {
        fprintf(stderr, "cannot create keyword stream\n");
        goto cleanup;
    }
    if (argc == 4) {
        // Open a capture FIFO only after model initialization. Its producer then
        // starts recording without accumulating audio during model startup.
        input = fopen(argv[3], "rb");
        if (!input) {
            perror("open PCM input");
            goto cleanup;
        }
    }
    if ((argc == 3 && strcmp(argv[2], "--raw-stdin") != 0
             ? read_wave(&r, argv[2]) : read_raw(&r, input)) != 0)
        goto cleanup;
    if (!r.samples) {
        fprintf(stderr, "empty audio input\n");
        goto cleanup;
    }
    const float silence[CHUNK] = {0};
    for (int n = 0; n < TAIL; n += CHUNK)
        if (feed(&r, silence, CHUNK) != 0)
            goto cleanup;
    SherpaOnnxOnlineStreamInputFinished(r.stream);
    if (decode(&r) == 0)
        status = 0;

cleanup:
    if (input && input != stdin)
        fclose(input);
    if (r.stream)
        SherpaOnnxDestroyOnlineStream(r.stream);
    if (r.spotter)
        SherpaOnnxDestroyKeywordSpotter(r.spotter);
    for (int i = 0; i < 5; ++i)
        free(paths[i]);
    if (interrupted) {
        fprintf(stderr, "interrupted by signal %d\n", (int)interrupted);
        return 128 + interrupted;
    }
    return status;
}
