// SPDX-License-Identifier: Apache-2.0
// Direct ALSA lifecycle checks; WAV recording is handled by arecord.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <poll.h>
#include <sched.h>
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/select.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#include <sound/asound.h>

#define PCM_PATH "/dev/snd/pcmC0D0c"
#define CTL_PATH "/dev/snd/controlC0"
#define PERIOD_FRAMES 1024
#define BUFFER_FRAMES (8 * PERIOD_FRAMES)
#define CHECK(condition) do { \
    if (!(condition)) { \
        fprintf(stderr, "SG2002_AUDIO_FAILED: line %d: %s (errno=%d)\n", \
                __LINE__, #condition, errno); \
        exit(1); \
    } \
} while (0)

// Independent Linux C-header checks, not copies of the Rust structures.
_Static_assert(sizeof(long) == 8, "LP64 is required");
_Static_assert(__BYTE_ORDER__ == __ORDER_LITTLE_ENDIAN__, "little endian is required");
_Static_assert(sizeof(struct snd_pcm_info) == 288, "PCM info layout");
_Static_assert(sizeof(struct snd_pcm_hw_params) == 608, "HW params layout");
_Static_assert(offsetof(struct snd_pcm_hw_params, intervals) == 260, "intervals offset");
_Static_assert(sizeof(struct snd_pcm_sw_params) == 136, "SW params layout");
_Static_assert(sizeof(struct snd_pcm_status) == 152, "status layout");
_Static_assert(offsetof(struct snd_pcm_status, audio_tstamp) == 96, "timestamp offset");
_Static_assert(sizeof(struct snd_pcm_sync_ptr) == 136, "sync ptr layout");
_Static_assert(offsetof(struct snd_pcm_sync_ptr, c) == 72, "sync control offset");
_Static_assert(sizeof(struct snd_xferi) == 24, "frame transfer layout");
_Static_assert(sizeof(struct snd_ctl_card_info) == 376, "card info layout");
_Static_assert(sizeof(struct snd_ctl_elem_list) == 80, "control list layout");
_Static_assert(sizeof(struct snd_ctl_elem_info) == 272, "control info layout");
_Static_assert(sizeof(struct snd_ctl_elem_value) == 1224, "control value layout");
_Static_assert(offsetof(struct snd_ctl_elem_value, value) == 72, "control value offset");
_Static_assert(SNDRV_PCM_IOCTL_READI_FRAMES == 0x80184151UL, "readi command");
_Static_assert(SNDRV_PCM_IOCTL_STATUS == 0x80984120UL, "status command");
_Static_assert(SNDRV_CTL_IOCTL_ELEM_READ == 0xc4c85512UL, "control command");

static int pcm_ioctl(int fd, unsigned long command, void *argument)
{
    return (int)syscall(SYS_ioctl, fd, command, argument);
}

static void set_interval(struct snd_pcm_hw_params *p, int parameter, unsigned value)
{
    p->intervals[parameter - SNDRV_PCM_HW_PARAM_FIRST_INTERVAL] =
        (struct snd_interval) { .min = value, .max = value, .integer = 1 };
}

static struct snd_pcm_hw_params hardware_params(unsigned rate)
{
    struct snd_pcm_hw_params p = {0};
    p.masks[SNDRV_PCM_HW_PARAM_ACCESS].bits[0] = 1U << SNDRV_PCM_ACCESS_RW_INTERLEAVED;
    p.masks[SNDRV_PCM_HW_PARAM_FORMAT].bits[0] = 1U << SNDRV_PCM_FORMAT_S16_LE;
    p.masks[SNDRV_PCM_HW_PARAM_SUBFORMAT].bits[0] = 1U << SNDRV_PCM_SUBFORMAT_STD;
    for (size_t i = 0; i < sizeof(p.intervals) / sizeof(p.intervals[0]); ++i)
        p.intervals[i].max = UINT_MAX;
    p.rmask = UINT_MAX;
    set_interval(&p, SNDRV_PCM_HW_PARAM_CHANNELS, 1);
    set_interval(&p, SNDRV_PCM_HW_PARAM_RATE, rate);
    set_interval(&p, SNDRV_PCM_HW_PARAM_PERIOD_SIZE, PERIOD_FRAMES);
    set_interval(&p, SNDRV_PCM_HW_PARAM_BUFFER_SIZE, BUFFER_FRAMES);
    return p;
}

static void configure(int fd, unsigned rate)
{
    struct snd_pcm_hw_params p = hardware_params(rate);
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_HW_REFINE, &p) == 0);
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_HW_PARAMS, &p) == 0);
    struct snd_pcm_sw_params sw = {
        .period_step = 1, .avail_min = PERIOD_FRAMES, .xfer_align = 1,
        .start_threshold = 1, .stop_threshold = BUFFER_FRAMES, .boundary = BUFFER_FRAMES,
    };
    while (sw.boundary <= (LONG_MAX - BUFFER_FRAMES) / 2UL)
        sw.boundary *= 2;
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_SW_PARAMS, &sw) == 0);
}

static int open_capture(void)
{
    int fd = open(PCM_PATH, O_RDONLY | O_NONBLOCK | O_CLOEXEC);
    CHECK(fd >= 0);
    CHECK(syscall(SYS_readv, fd, NULL, 0) == 0);
    int protocol;
    struct snd_pcm_info info = {0};
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_PVERSION, &protocol) == 0);
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_INFO, &info) == 0);
    CHECK(info.stream == SNDRV_PCM_STREAM_CAPTURE);
    // alsa-lib falls back to SYNC_PTR before configuring hardware.
    struct snd_pcm_sync_ptr sync = {
        .flags = SNDRV_PCM_SYNC_PTR_APPL | SNDRV_PCM_SYNC_PTR_AVAIL_MIN,
    };
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_SYNC_PTR, &sync) == 0);
    CHECK(sync.s.status.state == SNDRV_PCM_STATE_OPEN);
    return fd;
}

static void start_capture(int fd)
{
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_PREPARE, NULL) == 0);
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_START, NULL) == 0);
}

static int64_t monotonic_ms(void)
{
    struct timespec t;
    CHECK(clock_gettime(CLOCK_MONOTONIC, &t) == 0);
    return (int64_t)t.tv_sec * 1000 + t.tv_nsec / 1000000;
}

static void capture_one_second(int fd, unsigned rate)
{
    start_capture(fd);
    int64_t deadline = monotonic_ms() + 6000;
    unsigned remaining = rate;
    while (remaining) {
        int16_t samples[PERIOD_FRAMES];
        unsigned requested = remaining < PERIOD_FRAMES ? remaining : PERIOD_FRAMES;
        struct snd_xferi transfer = { .buf = samples, .frames = requested };
        int result = pcm_ioctl(fd, SNDRV_PCM_IOCTL_READI_FRAMES, &transfer);
        int error = errno;
        int64_t left = deadline - monotonic_ms();
        CHECK(left > 0);
        if (result < 0) {
            if (error == EINTR)
                continue;
            CHECK(error == EAGAIN);
            struct pollfd wait = { .fd = fd, .events = POLLIN };
            int ready = poll(&wait, 1, (int)left);
            if (ready < 0 && errno == EINTR)
                continue;
            CHECK(ready > 0 && !(wait.revents & (POLLERR | POLLHUP | POLLNVAL)));
            continue;
        }
        CHECK(transfer.result > 0 && (unsigned long)transfer.result <= requested);
        remaining -= (unsigned)transfer.result;
    }
    struct snd_pcm_sync_ptr sync = {
        .flags = SNDRV_PCM_SYNC_PTR_HWSYNC | SNDRV_PCM_SYNC_PTR_APPL | SNDRV_PCM_SYNC_PTR_AVAIL_MIN,
    };
    struct snd_pcm_status status = {0};
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_SYNC_PTR, &sync) == 0);
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_STATUS, &status) == 0);
    CHECK(status.state == SNDRV_PCM_STATE_RUNNING && sync.s.status.state == SNDRV_PCM_STATE_RUNNING);
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_DROP, NULL) == 0);
}

static void interrupt_read(int signal_number)
{
    (void)signal_number;
}

static void wait_for_sleep(pid_t child)
{
    char path[64];
    snprintf(path, sizeof(path), "/proc/%ld/status", (long)child);
    int64_t deadline = monotonic_ms() + 3000;
    for (;;) {
        FILE *file = fopen(path, "r");
        CHECK(file != NULL);
        char line[128], state = 0;
        while (fgets(line, sizeof(line), file)) {
            if (sscanf(line, "State: %c", &state) == 1)
                break;
        }
        fclose(file);
        if (state == 'S')
            return;
        CHECK(state != 'Z' && state != 'X' && monotonic_ms() < deadline);
        sched_yield();
    }
}

static void check_blocking_and_overrun(int fd, unsigned rate)
{
    struct snd_pcm_sw_params sw = {
        .period_step = 1, .avail_min = BUFFER_FRAMES, .xfer_align = 1,
        .start_threshold = BUFFER_FRAMES + 1, .stop_threshold = BUFFER_FRAMES,
    };
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_SW_PARAMS, &sw) == 0);
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_PREPARE, NULL) == 0);
    int16_t samples[PERIOD_FRAMES];
    struct snd_xferi transfer = { .buf = samples, .frames = PERIOD_FRAMES };
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_READI_FRAMES, &transfer) < 0 && errno == EAGAIN);
    CHECK(transfer.result == -EAGAIN);
    CHECK(fcntl(fd, F_SETFL, 0) == 0);
    CHECK((fcntl(fd, F_GETFL) & O_NONBLOCK) == 0);
    struct sigaction action = { .sa_handler = interrupt_read }, previous;
    CHECK(sigaction(SIGALRM, &action, &previous) == 0);
    alarm(1); // No START: this read must be interrupted without consuming data.
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_READI_FRAMES, &transfer) < 0 && errno == EINTR);
    CHECK(transfer.result == -EINTR);
    CHECK(sigaction(SIGALRM, &previous, NULL) == 0);
    for (int change = 0; change < 3; ++change) {
        CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_SW_PARAMS, &sw) == 0);
        CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_PREPARE, NULL) == 0);
        pid_t reader = fork();
        CHECK(reader >= 0);
        if (reader == 0) {
            alarm(5);
            long result = change == 1 ? syscall(SYS_read, fd, samples, sizeof(samples))
                                     : pcm_ioctl(fd, SNDRV_PCM_IOCTL_READI_FRAMES, &transfer);
            _exit(result < 0 && errno == EBADFD &&
                  (change == 1 || transfer.result == -EBADFD) ? 0 : 1);
        }
        pid_t selector = fork();
        CHECK(selector >= 0);
        if (selector == 0) {
            alarm(5);
            fd_set readable;
            FD_ZERO(&readable);
            FD_SET(fd, &readable);
            long result = syscall(SYS_pselect6, fd + 1, &readable, NULL, NULL, NULL, NULL);
            _exit(result == 1 && FD_ISSET(fd, &readable) ? 0 : 1);
        }
        // No DMA is running: only a parameter-state change can release this read.
        wait_for_sleep(reader);
        wait_for_sleep(selector);
        if (change == 1) {
            CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_HW_FREE, NULL) == 0);
        } else {
            struct snd_pcm_hw_params params = hardware_params(change == 2 ? 96000 : rate);
            int result = pcm_ioctl(fd, SNDRV_PCM_IOCTL_HW_PARAMS, &params);
            CHECK(change == 2 ? result < 0 && errno == EINVAL : result == 0);
        }
        struct snd_pcm_status state = {0};
        CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_STATUS, &state) == 0);
        CHECK(state.state == (change == 0 ? SNDRV_PCM_STATE_SETUP : SNDRV_PCM_STATE_OPEN));
        pid_t children[] = { reader, selector };
        for (size_t i = 0; i < sizeof(children) / sizeof(children[0]); ++i) {
            int status;
            pid_t reaped;
            do {
                reaped = waitpid(children[i], &status, 0);
            } while (reaped < 0 && errno == EINTR);
            CHECK(reaped == children[i]);
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0);
        }
        struct pollfd ready = { .fd = fd, .events = POLLIN | POLLRDNORM };
        struct timespec immediate = {0};
        CHECK(syscall(SYS_ppoll, &ready, 1, &immediate, NULL, 0) == 1);
        CHECK(ready.revents == (POLLIN | POLLRDNORM | POLLERR));
        configure(fd, rate);
    }
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_SW_PARAMS, &sw) == 0);
    alarm(6);
    // Explicit START; each read is smaller than poll's avail_min.
    capture_one_second(fd, rate);
    alarm(0);
    CHECK(fcntl(fd, F_SETFL, O_NONBLOCK) == 0);
    start_capture(fd);
    struct snd_pcm_hw_params invalid = hardware_params(96000);
    // State rejection precedes parameter validation and preserves the stream.
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_HW_PARAMS, &invalid) < 0 && errno == EBADFD);
    struct pollfd wait = { .fd = fd }; // Observe overrun without consuming samples.
    CHECK(poll(&wait, 1, 6000) > 0 && (wait.revents & POLLERR));
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_READI_FRAMES, &transfer) < 0 && errno == EPIPE);
    CHECK(transfer.result == -EPIPE);
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_DROP, NULL) == 0);
    configure(fd, rate);
}

static void check_drain(int fd)
{
    int16_t samples[BUFFER_FRAMES];
    CHECK(fcntl(fd, F_SETFL, 0) == 0);
    start_capture(fd);
    struct pollfd readable = { .fd = fd, .events = POLLIN };
    CHECK(poll(&readable, 1, 3000) > 0 && readable.revents == POLLIN);
    pid_t reader = fork();
    CHECK(reader >= 0);
    if (reader == 0) {
        alarm(5);
        struct snd_xferi transfer = { .buf = samples, .frames = BUFFER_FRAMES };
        int result = pcm_ioctl(fd, SNDRV_PCM_IOCTL_READI_FRAMES, &transfer);
        _exit(result == 0 && transfer.result > 0 && transfer.result < BUFFER_FRAMES ? 0 : 1);
    }
    wait_for_sleep(reader);
    struct snd_pcm_status progress = {0};
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_STATUS, &progress) == 0);
    CHECK(progress.appl_ptr > 0); // The read has copied data and is waiting for more.
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_DRAIN, NULL) == 0);
    int child_status;
    pid_t waited;
    do { waited = waitpid(reader, &child_status, 0); } while (waited < 0 && errno == EINTR);
    CHECK(waited == reader && WIFEXITED(child_status) && WEXITSTATUS(child_status) == 0);
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_DROP, NULL) == 0);
    for (int scenario = 0; scenario < 3; ++scenario) {
        start_capture(fd);
        struct pollfd ready = { .fd = fd, .events = scenario == 2 ? 0 : POLLIN };
        CHECK(poll(&ready, 1, 6000) > 0);
        CHECK(ready.revents == (scenario == 2 ? POLLERR : POLLIN));
        if (scenario == 1)
            CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_DROP, NULL) == 0);
        CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_DRAIN, NULL) == 0);
        struct snd_pcm_status status = {0};
        CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_STATUS, &status) == 0);
        const int states[] = { SNDRV_PCM_STATE_DRAINING, SNDRV_PCM_STATE_SETUP, SNDRV_PCM_STATE_XRUN };
        CHECK(status.state == states[scenario]);
        struct snd_xferi transfer = { .buf = samples, .frames = BUFFER_FRAMES };
        int result = pcm_ioctl(fd, SNDRV_PCM_IOCTL_READI_FRAMES, &transfer);
        int error = scenario == 2 ? EPIPE : EBADFD;
        CHECK(result < 0 && errno == error && transfer.result == -error);
        CHECK(syscall(SYS_read, fd, samples, sizeof(samples)) < 0 && errno == error);
        CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_STATUS, &status) == 0);
        CHECK(status.state == states[scenario]);
        CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_DROP, NULL) == 0);
    }
    CHECK(fcntl(fd, F_SETFL, O_NONBLOCK) == 0);
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_DRAIN, NULL) < 0 && errno == EAGAIN);
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_PREPARE, NULL) == 0);
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_DRAIN, NULL) < 0 && errno == EAGAIN);
    struct snd_pcm_status prepared = {0};
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_STATUS, &prepared) == 0);
    CHECK(prepared.state == SNDRV_PCM_STATE_PREPARED);
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_DROP, NULL) == 0);
}

static void check_geometry(int fd, unsigned rate)
{
    struct snd_pcm_hw_params params = hardware_params(rate);
    set_interval(&params, SNDRV_PCM_HW_PARAM_PERIOD_SIZE, 64);
    set_interval(&params, SNDRV_PCM_HW_PARAM_BUFFER_SIZE, 128);
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_HW_REFINE, &params) < 0 && errno == EINVAL);
    set_interval(&params, SNDRV_PCM_HW_PARAM_BUFFER_SIZE, 192);
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_HW_PARAMS, &params) == 0);
    start_capture(fd);
    // Exercise repeated wraps at the smallest serviceable DMA geometry.
    for (int period = 0; period < 16; ++period) {
        struct pollfd ready = { .fd = fd, .events = POLLIN };
        CHECK(poll(&ready, 1, 3000) > 0 && ready.revents == POLLIN);
        int16_t samples[64];
        struct snd_xferi transfer = { .buf = samples, .frames = 64 };
        CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_READI_FRAMES, &transfer) == 0 && transfer.result == 64);
    }
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_DROP, NULL) == 0);
    configure(fd, rate);
}

static void expect_busy(void)
{
    int second = open(PCM_PATH, O_RDONLY | O_NONBLOCK | O_CLOEXEC);
    CHECK(second < 0 && errno == EBUSY);
}

static void check_lifetime(int fd)
{
    int duplicate = dup(fd);
    CHECK(duplicate >= 0);
    close(fd);
    expect_busy();

    int release[2];
    CHECK(pipe(release) == 0);
    pid_t child = fork();
    CHECK(child >= 0);
    if (child == 0) {
        close(release[1]);
        char byte;
        ssize_t n;
        do { n = read(release[0], &byte, 1); } while (n < 0 && errno == EINTR);
        // Exit releases the last inherited PCM reference, including active DMA.
        _exit(n == 1 ? 0 : 1);
    }
    close(release[0]);
    close(duplicate);
    expect_busy();
    ssize_t sent;
    do { sent = write(release[1], "x", 1); } while (sent < 0 && errno == EINTR);
    CHECK(sent == 1);
    close(release[1]);
    int status;
    pid_t waited;
    do { waited = waitpid(child, &status, 0); } while (waited < 0 && errno == EINTR);
    CHECK(waited == child && WIFEXITED(status) && WEXITSTATUS(status) == 0);
}

static void check_gain(void)
{
    int fd = open(CTL_PATH, O_RDWR | O_CLOEXEC);
    CHECK(fd >= 0);
    struct snd_ctl_card_info card = {0};
    struct snd_ctl_elem_info info = { .id.iface = SNDRV_CTL_ELEM_IFACE_MIXER };
    memcpy(info.id.name, "ADC Capture Volume", sizeof("ADC Capture Volume"));
    CHECK(pcm_ioctl(fd, SNDRV_CTL_IOCTL_CARD_INFO, &card) == 0);
    CHECK(pcm_ioctl(fd, SNDRV_CTL_IOCTL_ELEM_INFO, &info) == 0);
    CHECK(info.type == SNDRV_CTL_ELEM_TYPE_INTEGER && info.count == 1 &&
          info.value.integer.min == 0 && info.value.integer.max == 24);
    struct snd_ctl_elem_value original = { .id = info.id };
    CHECK(pcm_ioctl(fd, SNDRV_CTL_IOCTL_ELEM_READ, &original) == 0);
    CHECK(original.value.integer.value[0] >= 0 && original.value.integer.value[0] <= 24);

    struct snd_ctl_elem_value changed = original, readback = { .id = info.id };
    long expected = (original.value.integer.value[0] + 1) % 25;
    changed.value.integer.value[0] = expected;
    int written = pcm_ioctl(fd, SNDRV_CTL_IOCTL_ELEM_WRITE, &changed);
    int read = pcm_ioctl(fd, SNDRV_CTL_IOCTL_ELEM_READ, &readback);
    // Restore before checking results, even if a failed write changed hardware.
    CHECK(pcm_ioctl(fd, SNDRV_CTL_IOCTL_ELEM_WRITE, &original) == 0);
    close(fd);
    CHECK(written == 0 && read == 0 && readback.value.integer.value[0] == expected);
}

static void lifecycle(unsigned rate)
{
    check_gain();
    int fd = open_capture();
    struct snd_pcm_hw_params invalid = hardware_params(rate);
    set_interval(&invalid, SNDRV_PCM_HW_PARAM_CHANNELS, 2);
    CHECK(pcm_ioctl(fd, SNDRV_PCM_IOCTL_HW_REFINE, &invalid) < 0 && errno == EINVAL);
    expect_busy();
    configure(fd, rate);
    char sample[2];
    CHECK(syscall(SYS_read, fd, sample, 1) < 0 && errno == EINVAL);
    struct iovec channel = { .iov_base = sample, .iov_len = sizeof(sample) };
    CHECK(syscall(SYS_readv, fd, &channel, 1) < 0 && errno == EINVAL);
    check_geometry(fd, rate);
    check_drain(fd);
    check_blocking_and_overrun(fd, rate);
    capture_one_second(fd, rate);
    capture_one_second(fd, rate);
    start_capture(fd);
    check_lifetime(fd);
    fd = open_capture();
    configure(fd, rate);
    capture_one_second(fd, rate);
    close(fd);
}

int main(int argc, char **argv)
{
    if (argc == 2 && strcmp(argv[1], "--abi-check") == 0) {
        puts("ALSA_LP64_LAYOUT_OK");
        return 0;
    }
    if (argc != 3 ||
        (strcmp(argv[1], "--lifecycle") != 0 && strcmp(argv[1], "--blocking") != 0 &&
         strcmp(argv[1], "--drain") != 0 && strcmp(argv[1], "--geometry") != 0) ||
        (strcmp(argv[2], "16000") != 0 && strcmp(argv[2], "48000") != 0)) {
        fprintf(stderr, "Usage: %s --abi-check | {--lifecycle|--blocking|--drain|--geometry} {16000|48000}\n", argv[0]);
        return 2;
    }
    unsigned rate = strcmp(argv[2], "16000") == 0 ? 16000 : 48000;
    if (strcmp(argv[1], "--lifecycle") == 0) {
        lifecycle(rate);
    } else {
        int fd = open_capture();
        configure(fd, rate);
        if (strcmp(argv[1], "--blocking") == 0)
            check_blocking_and_overrun(fd, rate);
        else if (strcmp(argv[1], "--drain") == 0)
            check_drain(fd);
        else
            check_geometry(fd, rate);
        close(fd);
    }
    puts("SG2002_AUDIO_PASSED");
    return 0;
}
