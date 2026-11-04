#define _GNU_SOURCE

#include "sound_test.h"

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <poll.h>
#include <signal.h>
#include <sound/asound.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#ifndef SYS_close_range
#define SYS_close_range __NR_close_range
#endif

#define PROFILE "HEADLESS"
#define CONTROL_PATH "/dev/snd/controlC0"
#define PLAYBACK_PATH "/dev/snd/pcmC0D0p"
#define CAPTURE_PATH "/dev/snd/pcmC0D0c"

struct pcm_config {
    unsigned int period_frames;
    unsigned int buffer_frames;
};

static void sleep_ms(unsigned int milliseconds)
{
    struct timespec delay = {
        .tv_sec = milliseconds / 1000u,
        .tv_nsec = (long)(milliseconds % 1000u) * 1000L * 1000L,
    };

    while (nanosleep(&delay, &delay) < 0 && errno == EINTR) {
    }
}

static unsigned int buffer_wait_ms(const struct pcm_config *config)
{
    unsigned int buffer_ms =
        (config->buffer_frames * 1000u + SOUND_TEST_RATE - 1u) /
        SOUND_TEST_RATE;

    return buffer_ms * 2u + 100u;
}

static void pcm_status(int fd, struct snd_pcm_status *status)
{
    struct snd_pcm_status extended;

    memset(status, 0, sizeof(*status));
    sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_STATUS, status) == 0,
                       PROFILE, "STATUS: %s", strerror(errno));
    if (status->state == SNDRV_PCM_STATE_OPEN) {
        return;
    }
    memset(&extended, 0, sizeof(extended));
    sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_STATUS_EXT, &extended) == 0,
                       PROFILE, "STATUS_EXT: %s", strerror(errno));
    *status = extended;
}

static int pcm_state(int fd)
{
    struct snd_pcm_status status;

    pcm_status(fd, &status);
    return status.state;
}

static int reopen_pcm(const char *path)
{
    for (unsigned int attempt = 0; attempt < 100; ++attempt) {
        int fd = open(path, O_RDWR | O_NONBLOCK | O_CLOEXEC);

        if (fd >= 0) {
            return fd;
        }
        sound_test_require(errno == EBUSY, PROFILE,
                           "reopen %s returned errno %d, expected EBUSY",
                           path, errno);
        sleep_ms(10);
    }

    sound_test_fail(PROFILE, "%s remained busy after bounded reopen wait",
                    path);
}

static void drain_nonblocking_eagain(int fd)
{
    errno = 0;
    int result = ioctl(fd, SNDRV_PCM_IOCTL_DRAIN);

    sound_test_require(result < 0 && errno == EAGAIN, PROFILE,
                       "non-blocking DRAIN returned result=%d errno=%d",
                       result, errno);
}

static void drain_blocking_after_nonblock(int fd)
{
    int flags = fcntl(fd, F_GETFL);

    sound_test_require(flags >= 0 && (flags & O_NONBLOCK) != 0, PROFILE,
                       "blocking DRAIN probe requires O_NONBLOCK");
    sound_test_require(fcntl(fd, F_SETFL, flags & ~O_NONBLOCK) == 0, PROFILE,
                       "clear O_NONBLOCK for blocking DRAIN: %s",
                       strerror(errno));
    int cleared = fcntl(fd, F_GETFL);
    sound_test_require(cleared >= 0 && (cleared & O_NONBLOCK) == 0, PROFILE,
                       "DRAIN fd did not observe cleared O_NONBLOCK");
    sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_DRAIN) == 0, PROFILE,
                       "blocking DRAIN: %s", strerror(errno));
    sound_test_require(fcntl(fd, F_SETFL, flags) == 0, PROFILE,
                       "restore O_NONBLOCK after blocking DRAIN: %s",
                       strerror(errno));
    int restored = fcntl(fd, F_GETFL);
    sound_test_require(restored >= 0 && (restored & O_NONBLOCK) != 0, PROFILE,
                       "DRAIN fd did not observe restored O_NONBLOCK");
}

static void *pcm_mmap_syscall(int fd, size_t length, int protection,
                              off_t offset)
{
    /* Bypass libc's mmap pre-processing and pass the PCM ABI offset directly. */
    long mapped = syscall(SYS_mmap, (unsigned long)0, (unsigned long)length,
                          (unsigned long)protection,
                          (unsigned long)MAP_SHARED, (unsigned long)fd,
                          (unsigned long)offset);

    if (mapped == -1) {
        return MAP_FAILED;
    }
    if (mapped < 0 && mapped >= -4095L) {
        errno = (int)-mapped;
        return MAP_FAILED;
    }
    return (void *)(uintptr_t)mapped;
}

static void check_mmap_data_error(int fd, size_t page, int expected_errno)
{
    /* Offset zero selects DATA; one readable page and MAP_SHARED are valid. */
    errno = 0;
    void *data = pcm_mmap_syscall(
        fd, page, PROT_READ | PROT_WRITE, SNDRV_PCM_MMAP_OFFSET_DATA);
    int data_error = errno;

    sound_test_require(data == MAP_FAILED, PROFILE,
                       "PCM data mmap unexpectedly succeeded");
    sound_test_require(data_error == expected_errno, PROFILE,
                       "PCM data mmap returned errno %d, expected %d (%s)",
                       data_error, expected_errno, strerror(expected_errno));
}

static int check_optional_mmap(int fd, size_t page, int protection,
                               off_t offset, const char *what)
{
    errno = 0;
    void *mapping = pcm_mmap_syscall(fd, page, protection, offset);
    int mapped = mapping != MAP_FAILED;

    if (mapped) {
        sound_test_require(munmap(mapping, page) == 0, PROFILE,
                           "munmap PCM %s: %s", what, strerror(errno));
    } else {
        sound_test_require(errno == ENODEV || errno == ENXIO ||
                               errno == EINVAL || errno == ENOSYS,
                           PROFILE,
                           "PCM %s mmap returned errno %d before fallback",
                           what, errno);
    }
    return mapped;
}

static void check_mmap_and_sync_ptr(int fd, int expected_data_error)
{
    long page_size = sysconf(_SC_PAGESIZE);
    size_t page = page_size > 0 ? (size_t)page_size : 4096u;

    check_mmap_data_error(fd, page, expected_data_error);
    int status_mmap =
        check_optional_mmap(fd, page, PROT_READ,
                            (off_t)SNDRV_PCM_MMAP_OFFSET_STATUS_NEW, "status");
    (void)check_optional_mmap(
        fd, page, PROT_READ | PROT_WRITE,
        (off_t)SNDRV_PCM_MMAP_OFFSET_CONTROL_NEW, "control");

    struct snd_pcm_sync_ptr sync = {0};
    sync.flags =
        SNDRV_PCM_SYNC_PTR_APPL | SNDRV_PCM_SYNC_PTR_AVAIL_MIN;
    sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_SYNC_PTR, &sync) == 0,
                       PROFILE, "SYNC_PTR fallback: %s", strerror(errno));
    printf("VIRTIO_SOUND_MMAP data=rejected status=%s sync_ptr=ok\n",
           status_mmap ? "supported" : "fallback");
    fflush(stdout);
}

static void check_dup2_dup3_close_range(int fd)
{
    const int dup2_fd = 200;
    const int dup3_fd = 201;
    struct snd_pcm_status status;

    sound_test_require(dup2(fd, dup2_fd) == dup2_fd, PROFILE,
                       "dup2 PCM fd: %s", strerror(errno));
    sound_test_require(dup3(fd, dup3_fd, O_CLOEXEC) == dup3_fd, PROFILE,
                       "dup3 PCM fd: %s", strerror(errno));

    int flags = fcntl(dup2_fd, F_GETFD);
    sound_test_require(flags >= 0 && (flags & FD_CLOEXEC) == 0, PROFILE,
                       "dup2 unexpectedly set FD_CLOEXEC");
    flags = fcntl(dup3_fd, F_GETFD);
    sound_test_require(flags >= 0 && (flags & FD_CLOEXEC) != 0, PROFILE,
                       "dup3 did not set FD_CLOEXEC");

    flags = fcntl(dup2_fd, F_GETFL);
    sound_test_require(flags >= 0 && (flags & O_NONBLOCK) != 0, PROFILE,
                       "dup2 did not share O_NONBLOCK");
    sound_test_require(fcntl(dup2_fd, F_SETFL, flags & ~O_NONBLOCK) == 0,
                       PROFILE, "clear O_NONBLOCK through dup2: %s",
                       strerror(errno));
    int original_flags = fcntl(fd, F_GETFL);
    sound_test_require(original_flags >= 0 &&
                           (original_flags & O_NONBLOCK) == 0,
                       PROFILE, "dup2 did not share file status flags");
    sound_test_require(fcntl(dup3_fd, F_SETFL,
                             original_flags | O_NONBLOCK) == 0,
                       PROFILE, "restore O_NONBLOCK through dup3: %s",
                       strerror(errno));
    original_flags = fcntl(fd, F_GETFL);
    sound_test_require(original_flags >= 0 &&
                           (original_flags & O_NONBLOCK) != 0,
                       PROFILE, "dup3 did not share restored O_NONBLOCK");

    close(dup2_fd);
    pcm_status(fd, &status);

    long result = syscall(SYS_close_range, (unsigned int)dup3_fd,
                          (unsigned int)dup3_fd, 0u);
    sound_test_require(result == 0, PROFILE, "close_range alias: %s",
                       strerror(errno));
    pcm_status(fd, &status);
}

static void check_fork_inherits_fd(int fd)
{
    int pipe_fds[2];
    sound_test_require(pipe(pipe_fds) == 0, PROFILE, "fork pipe: %s",
                       strerror(errno));
    pid_t child = fork();
    sound_test_require(child >= 0, PROFILE, "fork inherited PCM: %s",
                       strerror(errno));
    if (child == 0) {
        struct snd_pcm_status status = {0};
        int flags = fcntl(fd, F_GETFL);
        char result =
            (flags >= 0 && (flags & O_NONBLOCK) != 0 &&
             ioctl(fd, SNDRV_PCM_IOCTL_STATUS, &status) == 0)
                ? 1
                : 0;

        close(pipe_fds[0]);
        if (write(pipe_fds[1], &result, sizeof(result)) != sizeof(result)) {
            _exit(2);
        }
        _exit(0);
    }

    close(pipe_fds[1]);
    struct pollfd ready = {.fd = pipe_fds[0], .events = POLLIN, .revents = 0};
    sound_test_require(poll(&ready, 1, 3000) == 1 &&
                           (ready.revents & POLLIN) != 0,
                       PROFILE, "forked PCM child did not report state");
    char result = 0;
    sound_test_require(read(pipe_fds[0], &result, sizeof(result)) ==
                           sizeof(result),
                       PROFILE, "read forked PCM child result");
    close(pipe_fds[0]);
    int status = sound_test_wait(child, 3000);
    sound_test_require(status == 0 && result == 1, PROFILE,
                       "forked PCM child status=%d inherited=%d", status,
                       (int)result);
}

static void check_control(void)
{
    int fd = open(CONTROL_PATH, O_RDWR | O_CLOEXEC);
    sound_test_require(fd >= 0, PROFILE, "open %s: %s", CONTROL_PATH,
                       strerror(errno));

    int version = 0;
    sound_test_require(ioctl(fd, SNDRV_CTL_IOCTL_PVERSION, &version) == 0,
                       PROFILE, "control PVERSION: %s", strerror(errno));
    sound_test_require(SNDRV_PROTOCOL_MAJOR(version) == 2, PROFILE,
                       "unexpected control protocol 0x%x", version);

    struct snd_ctl_card_info card = {0};
    sound_test_require(ioctl(fd, SNDRV_CTL_IOCTL_CARD_INFO, &card) == 0,
                       PROFILE, "control CARD_INFO: %s", strerror(errno));
    sound_test_require(card.card == 0 && card.id[0] != 0, PROFILE,
                       "unexpected card info card=%d id=%.16s", card.card,
                       (const char *)card.id);

    int device = -1;
    sound_test_require(ioctl(fd, SNDRV_CTL_IOCTL_PCM_NEXT_DEVICE, &device) == 0,
                       PROFILE, "control PCM_NEXT_DEVICE: %s",
                       strerror(errno));
    sound_test_require(device == 0, PROFILE,
                       "PCM_NEXT_DEVICE returned %d instead of 0", device);

    for (int stream = SNDRV_PCM_STREAM_PLAYBACK;
         stream <= SNDRV_PCM_STREAM_CAPTURE; ++stream) {
        struct snd_pcm_info info = {
            .device = 0,
            .subdevice = 0,
            .stream = stream,
        };

        sound_test_require(ioctl(fd, SNDRV_CTL_IOCTL_PCM_INFO, &info) == 0,
                           PROFILE, "control PCM_INFO stream %d: %s", stream,
                           strerror(errno));
        sound_test_require(info.card == 0 && info.device == 0 &&
                               info.stream == stream &&
                               info.subdevices_count >= 1,
                           PROFILE,
                           "control PCM_INFO stream %d has card=%d device=%d "
                           "stream=%d subdevices=%u",
                           stream, info.card, info.device, info.stream,
                           info.subdevices_count);
    }

    close(fd);
}

static void mask_set(struct snd_mask *mask, unsigned int value)
{
    memset(mask, 0, sizeof(*mask));
    mask->bits[value / 32u] |= (uint32_t)1u << (value % 32u);
}

static int mask_has(const struct snd_mask *mask, unsigned int value)
{
    return (mask->bits[value / 32u] & ((uint32_t)1u << (value % 32u))) != 0;
}

static struct snd_interval *param_interval(struct snd_pcm_hw_params *params,
                                           unsigned int param)
{
    return &params->intervals[param - SNDRV_PCM_HW_PARAM_FIRST_INTERVAL];
}

static void params_any(struct snd_pcm_hw_params *params)
{
    memset(params, 0, sizeof(*params));
    for (size_t index = 0;
         index < sizeof(params->masks) / sizeof(params->masks[0]); ++index) {
        memset(params->masks[index].bits, 0xff,
               sizeof(params->masks[index].bits));
    }
    for (size_t index = 0;
         index < sizeof(params->intervals) / sizeof(params->intervals[0]);
         ++index) {
        params->intervals[index].min = 0;
        params->intervals[index].max = UINT_MAX;
    }
    params->rmask = UINT_MAX;
}

static void interval_exact(struct snd_interval *interval, unsigned int value)
{
    interval->min = value;
    interval->max = value;
    interval->openmin = 0;
    interval->openmax = 0;
    interval->integer = 1;
    interval->empty = 0;
}

static int interval_has(const struct snd_interval *interval,
                        unsigned int value)
{
    return !interval->empty && value >= interval->min &&
           value <= interval->max;
}

static unsigned int interval_pick(const struct snd_interval *interval,
                                  unsigned int preferred)
{
    if (preferred < interval->min) {
        return interval->min;
    }
    if (preferred > interval->max) {
        return interval->max;
    }
    return preferred;
}

static snd_pcm_uframes_t canonical_boundary(unsigned int buffer_frames)
{
    snd_pcm_uframes_t boundary = buffer_frames;
    snd_pcm_uframes_t limit =
        (snd_pcm_uframes_t)LONG_MAX - (snd_pcm_uframes_t)buffer_frames;

    while (boundary <= limit / 2u) {
        boundary *= 2u;
    }
    return boundary;
}

struct capture_read_result {
    ssize_t count;
    int error;
};

static void check_capture_blocking_read_drop(int fd,
                                             const struct pcm_config *config)
{
    const unsigned int wait_ms = 3000;
    const unsigned int watchdog_ms = 5000;
    /*
     * Request far more capture data than one ring can hold.  A blocking
     * read keeps waiting until the whole request is satisfied, so this
     * stays blocked well beyond the watchdog interval at 48 kHz.
     */
    const size_t blocking_frames = (size_t)1u << 20;
    const size_t blocking_bytes =
        blocking_frames * SOUND_TEST_CHANNELS * sizeof(int16_t);
    snd_pcm_uframes_t boundary = canonical_boundary(config->buffer_frames);
    struct snd_pcm_sw_params sw = {0};
    int ready_pipe[2] = {-1, -1};
    int result_pipe[2] = {-1, -1};
    int done_pipe[2] = {-1, -1};
    int16_t *read_buffer = NULL;
    pid_t reader = -1;
    pid_t watchdog = -1;
    int flags = -1;
    int drop_result = 0;
    int drop_error = 0;
    int reader_status = -1;
    struct capture_read_result read_result = {0};
    char failure_detail[192];
    const char *failure = NULL;

    sw.tstamp_mode = SNDRV_PCM_TSTAMP_NONE;
    sw.period_step = 1;
    sw.avail_min = boundary;
    sw.start_threshold = 1;
    sw.stop_threshold = boundary;
    sw.boundary = 0;
    sw.proto = SNDRV_PCM_VERSION;
    int initial_state = pcm_state(fd);
    sound_test_require(initial_state == SNDRV_PCM_STATE_SETUP, PROFILE,
                       "capture blocking-read requires SETUP, state=%d",
                       initial_state);
    sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_SW_PARAMS, &sw) == 0,
                       PROFILE, "capture blocking-read SW_PARAMS: %s",
                       strerror(errno));
    sound_test_require(sw.boundary == boundary, PROFILE,
                       "capture blocking-read SW_PARAMS returned boundary %lu, "
                       "expected %lu",
                       (unsigned long)sw.boundary, (unsigned long)boundary);
    read_buffer =
        calloc(blocking_frames, SOUND_TEST_CHANNELS * sizeof(*read_buffer));
    sound_test_require(read_buffer != NULL, PROFILE,
                       "allocate blocking capture read buffer");

    flags = fcntl(fd, F_GETFL);
    sound_test_require(flags >= 0 && (flags & O_NONBLOCK) != 0, PROFILE,
                       "capture blocking-read requires O_NONBLOCK initially");
    sound_test_require(fcntl(fd, F_SETFL, flags & ~O_NONBLOCK) == 0, PROFILE,
                       "clear O_NONBLOCK for blocking capture read: %s",
                       strerror(errno));
    int cleared = fcntl(fd, F_GETFL);
    sound_test_require(cleared >= 0 && (cleared & O_NONBLOCK) == 0, PROFILE,
                       "capture read fd did not observe cleared O_NONBLOCK");

    /*
     * Linux latches O_NONBLOCK into substream->f_flags in PREPARE, and
     * read() consults that copy. PREPARE must therefore follow the fcntl
     * so the inherited open file description really blocks in read().
     */
    sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_PREPARE) == 0, PROFILE,
                       "capture blocking-read PREPARE: %s", strerror(errno));
    sound_test_require(pcm_state(fd) == SNDRV_PCM_STATE_PREPARED, PROFILE,
                       "capture blocking-read PREPARE is not PREPARED");

    sound_test_require(pipe(ready_pipe) == 0, PROFILE,
                       "capture blocking-read ready pipe: %s", strerror(errno));
    sound_test_require(pipe(result_pipe) == 0, PROFILE,
                       "capture blocking-read result pipe: %s", strerror(errno));
    sound_test_require(pipe(done_pipe) == 0, PROFILE,
                       "capture blocking-read watchdog pipe: %s",
                       strerror(errno));
    pid_t owner = getpid();
    watchdog = fork();
    sound_test_require(watchdog >= 0, PROFILE,
                       "fork capture DROP watchdog: %s", strerror(errno));
    if (watchdog == 0) {
        close(done_pipe[1]);
        close(ready_pipe[0]);
        close(ready_pipe[1]);
        close(result_pipe[0]);
        close(result_pipe[1]);
        close(fd);
        struct pollfd done = {.fd = done_pipe[0], .events = POLLIN, .revents = 0};
        int completed = poll(&done, 1, watchdog_ms);

        if (completed == 0 && getppid() == owner) {
            static const char message[] =
                "VIRTIO_SOUND_HEADLESS_FAILED: blocking capture read/DROP "
                "watchdog timeout\n";
            (void)write(STDOUT_FILENO, message, sizeof(message) - 1);
            (void)kill(owner, SIGKILL);
        }
        _exit(0);
    }
    close(done_pipe[0]);
    done_pipe[0] = -1;

    reader = fork();
    if (reader < 0) {
        failure = "fork blocking capture reader";
        goto cleanup;
    }
    if (reader == 0) {
        close(ready_pipe[0]);
        close(result_pipe[0]);
        close(done_pipe[0]);
        close(done_pipe[1]);
        char ready = 'R';
        if (write(ready_pipe[1], &ready, sizeof(ready)) !=
            (ssize_t)sizeof(ready)) {
            _exit(2);
        }
        close(ready_pipe[1]);

        errno = 0;
        ssize_t count = read(fd, read_buffer, blocking_bytes);
        struct capture_read_result result = {
            .count = count,
            .error = count < 0 ? errno : 0,
        };
        (void)write(result_pipe[1], &result, sizeof(result));
        _exit(0);
    }
    close(ready_pipe[1]);
    ready_pipe[1] = -1;
    close(result_pipe[1]);
    result_pipe[1] = -1;

    struct pollfd ready = {.fd = ready_pipe[0], .events = POLLIN, .revents = 0};
    if (poll(&ready, 1, wait_ms) != 1 || (ready.revents & POLLIN) == 0) {
        failure = "blocking capture reader did not reach read";
        goto cleanup;
    }
    char ready_byte;
    if (read(ready_pipe[0], &ready_byte, sizeof(ready_byte)) !=
        (ssize_t)sizeof(ready_byte)) {
        failure = "blocking capture reader handshake failed";
        goto cleanup;
    }
    close(ready_pipe[0]);
    ready_pipe[0] = -1;

    /*
     * A large capture read auto-starts the PREPARED stream.  Observe
     * RUNNING after the reader's handshake to prove it entered the wait
     * path; this STATUS itself must not deadlock behind the read.
     */
    int running_state = -1;
    for (unsigned int attempt = 0; attempt < 300; ++attempt) {
        struct snd_pcm_status status;

        if (ioctl(fd, SNDRV_PCM_IOCTL_STATUS, &status) != 0) {
            int status_error = errno;

            snprintf(failure_detail, sizeof(failure_detail),
                     "blocking capture reader STATUS failed: errno=%d",
                     status_error);
            failure = failure_detail;
            goto cleanup;
        }
        running_state = status.state;
        if (running_state == SNDRV_PCM_STATE_RUNNING) {
            break;
        }
        if (running_state != SNDRV_PCM_STATE_PREPARED) {
            snprintf(failure_detail, sizeof(failure_detail),
                     "blocking capture reader entered state %d, expected "
                     "RUNNING",
                     running_state);
            failure = failure_detail;
            goto cleanup;
        }
        sleep_ms(10);
    }
    if (running_state != SNDRV_PCM_STATE_RUNNING) {
        snprintf(failure_detail, sizeof(failure_detail),
                 "blocking capture reader did not enter RUNNING, state=%d",
                 running_state);
        failure = failure_detail;
        goto cleanup;
    }

    struct pollfd early_result = {
        .fd = result_pipe[0],
        .events = POLLIN,
        .revents = 0,
    };
    int early_result_ready = poll(&early_result, 1, 0);
    if (early_result_ready < 0) {
        failure = "poll blocking capture read result failed";
        goto cleanup;
    }
    if (early_result_ready > 0) {
        struct capture_read_result early = {0};

        if (read(result_pipe[0], &early, sizeof(early)) ==
            (ssize_t)sizeof(early)) {
            snprintf(failure_detail, sizeof(failure_detail),
                     "blocking capture read returned before DROP: "
                     "count=%zd errno=%d",
                     early.count, early.error);
            failure = failure_detail;
        } else {
            failure = "blocking capture read returned before DROP";
        }
        goto cleanup;
    }

    errno = 0;
    drop_result = ioctl(fd, SNDRV_PCM_IOCTL_DROP);
    drop_error = drop_result < 0 ? errno : 0;
    if (drop_result < 0) {
        snprintf(failure_detail, sizeof(failure_detail),
                 "blocking capture DROP failed: errno=%d", drop_error);
        failure = failure_detail;
        goto cleanup;
    }

    struct pollfd result_ready = {
        .fd = result_pipe[0],
        .events = POLLIN,
        .revents = 0,
    };
    if (poll(&result_ready, 1, wait_ms) != 1 ||
        (result_ready.revents & POLLIN) == 0) {
        failure = "blocking capture read did not return after DROP";
        goto cleanup;
    }
    ssize_t result_len =
        read(result_pipe[0], &read_result, sizeof(read_result));
    close(result_pipe[0]);
    result_pipe[0] = -1;
    if (result_len != (ssize_t)sizeof(read_result)) {
        failure = "blocking capture read result was incomplete";
        goto cleanup;
    }
    reader_status = sound_test_wait(reader, wait_ms);
    reader = -1;

    if (fcntl(fd, F_SETFL, flags) != 0) {
        failure = "restore O_NONBLOCK after blocking capture read";
        goto cleanup;
    }
    int restored = fcntl(fd, F_GETFL);
    if (restored < 0 || (restored & O_NONBLOCK) == 0) {
        failure = "capture read fd did not observe restored O_NONBLOCK";
        goto cleanup;
    }

    char done_signal = 'D';
    (void)write(done_pipe[1], &done_signal, sizeof(done_signal));
    close(done_pipe[1]);
    done_pipe[1] = -1;
    (void)sound_test_wait(watchdog, wait_ms);
    watchdog = -1;

    sound_test_require(reader_status == 0, PROFILE,
                       "blocking capture reader exited with status %d",
                       reader_status);
    sound_test_require(read_result.count < 0 && read_result.error == EBADFD,
                       PROFILE,
                       "blocking capture read returned count=%zd errno=%d, "
                       "expected EBADFD",
                       read_result.count, read_result.error);
    int after_drop_state = pcm_state(fd);
    sound_test_require(after_drop_state == SNDRV_PCM_STATE_SETUP, PROFILE,
                       "blocking capture DROP state=%d, expected SETUP",
                       after_drop_state);
    free(read_buffer);
    return;

cleanup:
    if (reader > 0) {
        (void)kill(reader, SIGKILL);
        (void)sound_test_wait(reader, 1000);
    }
    if (flags >= 0) {
        (void)fcntl(fd, F_SETFL, flags);
    }
    if (watchdog > 0) {
        if (done_pipe[1] >= 0) {
            char stop_watchdog = 'D';
            (void)write(done_pipe[1], &stop_watchdog, sizeof(stop_watchdog));
        }
        (void)sound_test_wait(watchdog, wait_ms);
    }
    free(read_buffer);
    for (size_t index = 0; index < 2; ++index) {
        if (ready_pipe[index] >= 0) {
            close(ready_pipe[index]);
        }
        if (result_pipe[index] >= 0) {
            close(result_pipe[index]);
        }
        if (done_pipe[index] >= 0) {
            close(done_pipe[index]);
        }
    }
    sound_test_fail(PROFILE, "%s",
                    failure != NULL ? failure
                                    : "blocking capture read/DROP failed");
}

static void expect_hw_params_failure(int fd, struct snd_pcm_hw_params *params,
                                     const char *what)
{
    errno = 0;
    int result = ioctl(fd, SNDRV_PCM_IOCTL_HW_PARAMS, params);

    sound_test_require(result < 0, PROFILE, "HW_PARAMS accepted %s", what);
    sound_test_require(errno == EINVAL, PROFILE,
                       "HW_PARAMS %s returned errno %d, expected EINVAL", what,
                       errno);
}

static struct pcm_config configure_pcm(int fd, int stream)
{
    struct pcm_config config = {0};
    int version = 0;

    sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_PVERSION, &version) == 0,
                       PROFILE, "PCM PVERSION: %s", strerror(errno));
    sound_test_require(SNDRV_PROTOCOL_MAJOR(version) == 2, PROFILE,
                       "unexpected PCM protocol 0x%x", version);

    struct snd_pcm_info info = {
        .device = 0,
        .subdevice = 0,
        .stream = stream,
    };
    sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_INFO, &info) == 0, PROFILE,
                       "PCM INFO stream %d: %s", stream, strerror(errno));
    sound_test_require(info.stream == stream && info.subdevices_count >= 1 &&
                           info.device == 0,
                       PROFILE,
                       "PCM INFO stream %d has device=%u stream=%d "
                       "subdevices=%u",
                       stream, info.device, info.stream,
                       info.subdevices_count);

    struct snd_pcm_hw_params refined;
    params_any(&refined);
    sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_HW_REFINE, &refined) == 0,
                       PROFILE, "HW_REFINE stream %d: %s", stream,
                       strerror(errno));
    sound_test_require(mask_has(&refined.masks[SNDRV_PCM_HW_PARAM_ACCESS],
                                SNDRV_PCM_ACCESS_RW_INTERLEAVED),
                       PROFILE, "RW_INTERLEAVED access is not advertised");
    sound_test_require(mask_has(&refined.masks[SNDRV_PCM_HW_PARAM_FORMAT],
                                SNDRV_PCM_FORMAT_S16_LE),
                       PROFILE, "S16_LE is not advertised");
    sound_test_require(
        interval_has(param_interval(&refined, SNDRV_PCM_HW_PARAM_CHANNELS), 2),
        PROFILE, "stereo capture/playback is not advertised");
    sound_test_require(
        interval_has(param_interval(&refined, SNDRV_PCM_HW_PARAM_RATE), 48000),
        PROFILE, "48000 Hz is not advertised");

    struct snd_pcm_hw_params invalid = refined;
    mask_set(&invalid.masks[SNDRV_PCM_HW_PARAM_FORMAT],
             SNDRV_PCM_FORMAT_S32_LE);
    invalid.rmask = UINT_MAX;
    expect_hw_params_failure(fd, &invalid, "S32_LE format");

    invalid = refined;
    interval_exact(param_interval(&invalid, SNDRV_PCM_HW_PARAM_RATE), 44100);
    invalid.rmask = UINT_MAX;
    expect_hw_params_failure(fd, &invalid, "44100 Hz rate");

    invalid = refined;
    interval_exact(param_interval(&invalid, SNDRV_PCM_HW_PARAM_CHANNELS), 1);
    invalid.rmask = UINT_MAX;
    expect_hw_params_failure(fd, &invalid, "mono channel count");

    invalid = refined;
    mask_set(&invalid.masks[SNDRV_PCM_HW_PARAM_ACCESS],
             SNDRV_PCM_ACCESS_MMAP_INTERLEAVED);
    invalid.rmask = UINT_MAX;
    expect_hw_params_failure(fd, &invalid, "mmap access");

    struct snd_pcm_hw_params params = refined;
    mask_set(&params.masks[SNDRV_PCM_HW_PARAM_ACCESS],
             SNDRV_PCM_ACCESS_RW_INTERLEAVED);
    mask_set(&params.masks[SNDRV_PCM_HW_PARAM_FORMAT],
             SNDRV_PCM_FORMAT_S16_LE);
    mask_set(&params.masks[SNDRV_PCM_HW_PARAM_SUBFORMAT],
             SNDRV_PCM_SUBFORMAT_STD);
    interval_exact(param_interval(&params, SNDRV_PCM_HW_PARAM_SAMPLE_BITS), 16);
    interval_exact(param_interval(&params, SNDRV_PCM_HW_PARAM_FRAME_BITS), 32);
    interval_exact(param_interval(&params, SNDRV_PCM_HW_PARAM_CHANNELS), 2);
    interval_exact(param_interval(&params, SNDRV_PCM_HW_PARAM_RATE), 48000);
    params.rmask = UINT_MAX;
    sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_HW_REFINE, &params) == 0,
                       PROFILE, "HW_REFINE exact stream %d: %s", stream,
                       strerror(errno));
    struct snd_interval *refined_period =
        param_interval(&params, SNDRV_PCM_HW_PARAM_PERIOD_SIZE);
    struct snd_interval *refined_buffer =
        param_interval(&params, SNDRV_PCM_HW_PARAM_BUFFER_SIZE);
    sound_test_require(!refined_period->empty && !refined_buffer->empty,
                       PROFILE, "HW_REFINE produced an empty period range");

    config.period_frames = interval_pick(refined_period, 1024);
    sound_test_require(config.period_frames >= 16, PROFILE,
                       "period size %u is too small",
                       config.period_frames);
    interval_exact(param_interval(&params, SNDRV_PCM_HW_PARAM_PERIOD_SIZE),
                   config.period_frames);
    config.buffer_frames =
        interval_pick(refined_buffer, config.period_frames * 4u);
    sound_test_require(config.buffer_frames >= config.period_frames * 2u,
                       PROFILE, "buffer %u is too small for period %u",
                       config.buffer_frames, config.period_frames);
    interval_exact(param_interval(&params, SNDRV_PCM_HW_PARAM_BUFFER_SIZE),
                   config.buffer_frames);
    params.rmask = UINT_MAX;

    sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_HW_PARAMS, &params) == 0,
                       PROFILE, "HW_PARAMS stream %d: %s", stream,
                       strerror(errno));
    sound_test_require(mask_has(&params.masks[SNDRV_PCM_HW_PARAM_ACCESS],
                                SNDRV_PCM_ACCESS_RW_INTERLEAVED) &&
                           mask_has(&params.masks[SNDRV_PCM_HW_PARAM_FORMAT],
                                    SNDRV_PCM_FORMAT_S16_LE),
                       PROFILE, "HW_PARAMS changed access or format");

    struct snd_interval *period =
        param_interval(&params, SNDRV_PCM_HW_PARAM_PERIOD_SIZE);
    struct snd_interval *buffer =
        param_interval(&params, SNDRV_PCM_HW_PARAM_BUFFER_SIZE);
    sound_test_require(period->min == period->max &&
                           buffer->min == buffer->max &&
                           buffer->min >= period->min * 2u,
                       PROFILE,
                       "HW_PARAMS returned period=%u..%u buffer=%u..%u",
                       period->min, period->max, buffer->min, buffer->max);
    config.period_frames = period->min;
    config.buffer_frames = buffer->min;

    struct snd_pcm_sw_params sw = {0};
    sw.tstamp_mode = SNDRV_PCM_TSTAMP_NONE;
    sw.period_step = 1;
    sw.avail_min = config.period_frames;
    sw.xfer_align = 1;
    sw.start_threshold = config.buffer_frames;
    sw.stop_threshold = config.buffer_frames;
    sw.boundary = 0;
    sw.proto = SNDRV_PCM_VERSION;
    sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_SW_PARAMS, &sw) == 0,
                       PROFILE, "SW_PARAMS stream %d: %s", stream,
                       strerror(errno));

    snd_pcm_uframes_t canonical = sw.boundary;
    snd_pcm_uframes_t expected =
        canonical_boundary(config.buffer_frames);
    sound_test_require(canonical == expected &&
                           canonical >= config.buffer_frames &&
                           canonical % config.buffer_frames == 0,
                       PROFILE,
                       "SW_PARAMS returned boundary %lu for buffer %u, "
                       "expected canonical %lu",
                       (unsigned long)canonical, config.buffer_frames,
                       (unsigned long)expected);

    sw.boundary = (snd_pcm_uframes_t)0x100000000ull;
    sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_SW_PARAMS, &sw) == 0,
                       PROFILE, "SW_PARAMS arbitrary boundary: %s",
                       strerror(errno));
    sound_test_require(sw.boundary == canonical, PROFILE,
                       "SW_PARAMS input boundary changed canonical value "
                       "from %lu to %lu",
                       (unsigned long)canonical, (unsigned long)sw.boundary);

    struct snd_pcm_status before_invalid_sw;
    pcm_status(fd, &before_invalid_sw);
    struct snd_pcm_sw_params bad_sw = sw;
    bad_sw.avail_min = 0;
    errno = 0;
    sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_SW_PARAMS, &bad_sw) < 0 &&
                           errno == EINVAL,
                       PROFILE, "SW_PARAMS accepted zero avail_min");
    struct snd_pcm_status after_invalid_sw;
    pcm_status(fd, &after_invalid_sw);
    sound_test_require(
        after_invalid_sw.state == before_invalid_sw.state &&
            after_invalid_sw.appl_ptr == before_invalid_sw.appl_ptr &&
            after_invalid_sw.hw_ptr == before_invalid_sw.hw_ptr,
        PROFILE,
        "invalid SW_PARAMS changed state or pointers (state %d -> %d, "
        "appl %lu -> %lu, hw %lu -> %lu)",
        before_invalid_sw.state, after_invalid_sw.state,
        (unsigned long)before_invalid_sw.appl_ptr,
        (unsigned long)after_invalid_sw.appl_ptr,
        (unsigned long)before_invalid_sw.hw_ptr,
        (unsigned long)after_invalid_sw.hw_ptr);

    return config;
}

static snd_pcm_sframes_t transfer(int fd, int command, void *buffer,
                                  snd_pcm_uframes_t frames)
{
    struct snd_xferi transfer = {
        .result = 0,
        .buf = buffer,
        .frames = frames,
    };

    sound_test_require(ioctl(fd, command, &transfer) == 0, PROFILE,
                       "PCM transfer command 0x%x frames=%lu: %s", command,
                       (unsigned long)frames, strerror(errno));
    return transfer.result;
}

static void check_dup_flags(int fd)
{
    int duplicate = dup(fd);

    sound_test_require(duplicate >= 0, PROFILE, "dup PCM fd: %s",
                       strerror(errno));
    int flags = fcntl(duplicate, F_GETFL);
    sound_test_require(flags >= 0 && (flags & O_NONBLOCK) != 0, PROFILE,
                       "dup did not preserve O_NONBLOCK");
    sound_test_require(fcntl(duplicate, F_SETFL, flags & ~O_NONBLOCK) == 0,
                       PROFILE, "clear O_NONBLOCK through dup: %s",
                       strerror(errno));
    int original_flags = fcntl(fd, F_GETFL);
    sound_test_require(original_flags >= 0 &&
                           (original_flags & O_NONBLOCK) == 0,
                       PROFILE, "dup did not share file status flags");
    sound_test_require(fcntl(fd, F_SETFL, original_flags | O_NONBLOCK) == 0,
                       PROFILE, "restore O_NONBLOCK: %s", strerror(errno));
    close(duplicate);
}

static void check_duplicate_open(const char *path)
{
    int duplicate = open(path, O_RDWR | O_NONBLOCK | O_CLOEXEC);

    if (duplicate >= 0) {
        close(duplicate);
        sound_test_fail(PROFILE, "duplicate open of %s unexpectedly succeeded",
                        path);
    }
    sound_test_require(errno == EBUSY, PROFILE,
                       "duplicate open of %s returned errno %d, expected EBUSY",
                       path, errno);
}

static void check_playback(void)
{
    int fd = open(PLAYBACK_PATH, O_RDWR | O_NONBLOCK | O_CLOEXEC);
    sound_test_require(fd >= 0, PROFILE, "open %s: %s", PLAYBACK_PATH,
                       strerror(errno));
    check_duplicate_open(PLAYBACK_PATH);
    check_dup_flags(fd);
    struct snd_pcm_status before_version_ioctls;
    pcm_status(fd, &before_version_ioctls);

    int legacy_tstamp = 1;
    errno = 0;
    sound_test_require(
        ioctl(fd, SNDRV_PCM_IOCTL_TSTAMP, (void *)(uintptr_t)1) == 0,
        PROFILE, "compat TSTAMP with unreadable arg returned errno %d",
        errno);
    sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_TSTAMP, &legacy_tstamp) == 0,
                       PROFILE, "compat TSTAMP returned errno %d", errno);
    sound_test_require(legacy_tstamp == 1, PROFILE,
                       "compat TSTAMP modified its legal int input");

    unsigned int user_version = 0;
    const unsigned int future_version =
        (unsigned int)SNDRV_PROTOCOL_VERSION(3, 0, 0);

    sound_test_require(
        ioctl(fd, SNDRV_PCM_IOCTL_USER_PVERSION, &user_version) == 0,
        PROFILE, "USER_PVERSION major 0: %s", strerror(errno));
    sound_test_require(user_version == 0, PROFILE,
                       "USER_PVERSION modified its input buffer");
    user_version = future_version;
    sound_test_require(
        ioctl(fd, SNDRV_PCM_IOCTL_USER_PVERSION, &user_version) == 0,
        PROFILE, "USER_PVERSION future major: %s", strerror(errno));
    sound_test_require(user_version == future_version, PROFILE,
                       "USER_PVERSION modified its future-major input");
    errno = 0;
    sound_test_require(
        ioctl(fd, SNDRV_PCM_IOCTL_USER_PVERSION, (void *)(uintptr_t)1) < 0 &&
            errno == EFAULT,
        PROFILE, "USER_PVERSION bad pointer returned errno %d", errno);
    user_version = SNDRV_PCM_VERSION;
    sound_test_require(
        ioctl(fd, SNDRV_PCM_IOCTL_USER_PVERSION, &user_version) == 0,
        PROFILE, "restore USER_PVERSION: %s", strerror(errno));

    struct snd_pcm_status after_version_ioctls;
    pcm_status(fd, &after_version_ioctls);
    sound_test_require(
        after_version_ioctls.state == before_version_ioctls.state &&
            after_version_ioctls.appl_ptr == before_version_ioctls.appl_ptr &&
            after_version_ioctls.hw_ptr == before_version_ioctls.hw_ptr,
        PROFILE,
        "TSTAMP/USER_PVERSION changed state or pointers "
        "(state %d -> %d, appl %lu -> %lu, hw %lu -> %lu)",
        before_version_ioctls.state, after_version_ioctls.state,
        (unsigned long)before_version_ioctls.appl_ptr,
        (unsigned long)after_version_ioctls.appl_ptr,
        (unsigned long)before_version_ioctls.hw_ptr,
        (unsigned long)after_version_ioctls.hw_ptr);

    errno = 0;
    sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_DRAIN) < 0 &&
                           errno == EBADFD,
                       PROFILE,
                       "non-blocking DRAIN in OPEN returned errno %d, "
                       "expected EBADFD",
                       errno);
    int open_drain_state = pcm_state(fd);
    sound_test_require(open_drain_state == SNDRV_PCM_STATE_OPEN, PROFILE,
                       "DRAIN in OPEN changed state to %d",
                       open_drain_state);
    check_mmap_and_sync_ptr(fd, EBADFD);

    struct pcm_config config = configure_pcm(fd, SNDRV_PCM_STREAM_PLAYBACK);
    sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_PREPARE) == 0, PROFILE,
                       "playback PREPARE: %s", strerror(errno));
    sound_test_require(pcm_state(fd) == SNDRV_PCM_STATE_PREPARED, PROFILE,
                       "playback is not PREPARED");
    check_dup2_dup3_close_range(fd);
    check_fork_inherits_fd(fd);

    size_t sample_count =
        (size_t)config.buffer_frames * 2u * SOUND_TEST_CHANNELS;
    size_t period_bytes = (size_t)config.period_frames * 4u;
    size_t buffer_bytes = (size_t)config.buffer_frames * 4u;
    int16_t *samples = calloc(sample_count, sizeof(*samples));
    sound_test_require(samples != NULL, PROFILE, "allocate playback samples");

    snd_pcm_sframes_t result =
        transfer(fd, SNDRV_PCM_IOCTL_WRITEI_FRAMES, samples,
                 config.period_frames);
    sound_test_require(result == (snd_pcm_sframes_t)config.period_frames,
                       PROFILE, "playback WRITEI transferred %ld of %u frames",
                       (long)result, config.period_frames);
    check_mmap_and_sync_ptr(fd, ENXIO);
    sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_START) == 0, PROFILE,
                       "playback START: %s", strerror(errno));
    ssize_t count = write(fd, samples, period_bytes);
    sound_test_require(count == (ssize_t)period_bytes, PROFILE,
                       "playback write returned %zd of %zu bytes",
                       count, period_bytes);

    int saw_eagain = 0;
    struct snd_xferi full = {
        .buf = samples,
        .frames = config.period_frames,
    };
    for (unsigned int attempt = 0; attempt < 64; ++attempt) {
        errno = 0;
        if (ioctl(fd, SNDRV_PCM_IOCTL_WRITEI_FRAMES, &full) < 0) {
            sound_test_require(errno == EAGAIN, PROFILE,
                               "full playback buffer returned errno %d, "
                               "expected EAGAIN",
                               errno);
            saw_eagain = 1;
            break;
        }
    }
    sound_test_require(saw_eagain, PROFILE,
                       "non-blocking playback never reported EAGAIN");

    int saw_partial = 0;
    for (unsigned int attempt = 0; attempt < 8; ++attempt) {
        struct pollfd poll_fd = {.fd = fd, .events = POLLOUT, .revents = 0};

        sound_test_require(poll(&poll_fd, 1, 3000) == 1 &&
                               (poll_fd.revents & POLLOUT) != 0,
                           PROFILE, "playback poll did not become writable");
        errno = 0;
        count = write(fd, samples, buffer_bytes * 2u);
        if (count > 0) {
            sound_test_require(count % 4 == 0 &&
                                   count < (ssize_t)(buffer_bytes * 2u),
                               PROFILE,
                               "playback partial write returned %zd bytes",
                               count);
            saw_partial = 1;
            break;
        }
        sound_test_require(count < 0 && errno == EAGAIN, PROFILE,
                           "playback partial write returned %zd errno %d",
                           count, errno);
    }
    sound_test_require(saw_partial, PROFILE,
                       "non-blocking playback never returned a partial write");

    struct snd_pcm_status before_bad;
    struct snd_pcm_status after_bad;
    pcm_status(fd, &before_bad);
    errno = 0;
    count = write(fd, samples, 3);
    sound_test_require(count < 0 && errno == EINVAL, PROFILE,
                       "playback non-frame write returned %zd errno %d", count,
                       errno);
    pcm_status(fd, &after_bad);
    sound_test_require(after_bad.appl_ptr == before_bad.appl_ptr, PROFILE,
                       "playback non-frame write advanced appl_ptr");

    struct pollfd writable = {.fd = fd, .events = POLLOUT, .revents = 0};
    sound_test_require(poll(&writable, 1, 3000) == 1 &&
                           (writable.revents & POLLOUT) != 0,
                       PROFILE,
                       "playback EFAULT probe did not become writable");
    errno = 0;
    count = write(fd, (void *)(uintptr_t)1, 4);
    sound_test_require(count < 0 && errno == EFAULT, PROFILE,
                       "playback bad user buffer returned %zd errno %d", count,
                       errno);
    pcm_status(fd, &after_bad);
    sound_test_require(after_bad.appl_ptr == before_bad.appl_ptr, PROFILE,
                       "playback EFAULT advanced appl_ptr");

    struct pollfd poll_fd = {.fd = fd, .events = POLLOUT, .revents = 0};
    struct timespec timeout = {.tv_sec = 3, .tv_nsec = 0};
    sigset_t empty;
    sigemptyset(&empty);
    sound_test_require(ppoll(&poll_fd, 1, &timeout, &empty) == 1 &&
                           (poll_fd.revents & POLLOUT) != 0,
                       PROFILE, "playback ppoll did not become writable");

    struct snd_pcm_status before;
    pcm_status(fd, &before);
    snd_pcm_sframes_t delay = -1;
    sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_DELAY, &delay) == 0 &&
                           delay >= 0,
                       PROFILE, "playback DELAY: %s", strerror(errno));
    int progressed = 0;
    for (unsigned int attempt = 0; attempt < 200; ++attempt) {
        struct snd_pcm_status now = {0};

        if (ioctl(fd, SNDRV_PCM_IOCTL_STATUS, &now) == 0 &&
            (now.hw_ptr != before.hw_ptr ||
             (before.delay > 0 && now.delay < before.delay))) {
            progressed = 1;
            break;
        }
        sleep_ms(10);
    }
    sound_test_require(progressed, PROFILE,
                       "playback delay never advanced; DMA made no progress");

    sleep_ms(buffer_wait_ms(&config));
    int state_xrun = pcm_state(fd) == SNDRV_PCM_STATE_XRUN;
    errno = 0;
    count = write(fd, samples, period_bytes);
    sound_test_require(count < 0 && errno == EPIPE, PROFILE,
                       "playback underrun state=%d write=%zd errno=%d",
                       state_xrun, count, errno);
    sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_PREPARE) == 0, PROFILE,
                       "playback XRUN PREPARE recovery: %s", strerror(errno));
    sound_test_require(pcm_state(fd) == SNDRV_PCM_STATE_PREPARED, PROFILE,
                       "playback did not recover to PREPARED");

    count = write(fd, samples, period_bytes);
    sound_test_require(count == (ssize_t)period_bytes, PROFILE,
                       "playback pre-drain write returned %zd bytes", count);
    struct snd_pcm_status buffered;
    pcm_status(fd, &buffered);
    sound_test_require(buffered.state == SNDRV_PCM_STATE_PREPARED &&
                           buffered.appl_ptr != buffered.hw_ptr,
                       PROFILE,
                       "playback drain did not start from PREPARED with data "
                       "(state=%d appl=%lu hw=%lu)",
                       buffered.state, (unsigned long)buffered.appl_ptr,
                       (unsigned long)buffered.hw_ptr);
    drain_nonblocking_eagain(fd);
    int drain_state = pcm_state(fd);
    sound_test_require(
        drain_state == SNDRV_PCM_STATE_DRAINING ||
            drain_state == SNDRV_PCM_STATE_SETUP,
        PROFILE,
        "playback non-block DRAIN state=%d, expected DRAINING or SETUP",
        drain_state);
    int saw_setup = 0;
    unsigned int drain_wait = buffer_wait_ms(&config) + 1000u;
    for (unsigned int elapsed = 0; elapsed < drain_wait; elapsed += 10) {
        if (pcm_state(fd) == SNDRV_PCM_STATE_SETUP) {
            saw_setup = 1;
            break;
        }
        sleep_ms(10);
    }
    sound_test_require(saw_setup, PROFILE,
                       "playback DRAIN did not reach SETUP");

    sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_PREPARE) == 0, PROFILE,
                       "playback blocking-drain PREPARE: %s",
                       strerror(errno));
    count = write(fd, samples, period_bytes);
    sound_test_require(count == (ssize_t)period_bytes, PROFILE,
                       "playback blocking-drain write returned %zd bytes",
                       count);
    sound_test_require(pcm_state(fd) == SNDRV_PCM_STATE_PREPARED, PROFILE,
                       "playback blocking-drain data did not stay PREPARED");
    drain_blocking_after_nonblock(fd);
    sound_test_require(pcm_state(fd) == SNDRV_PCM_STATE_SETUP, PROFILE,
                       "playback blocking DRAIN left unexpected state");

    for (unsigned int repeat = 0; repeat < 2; ++repeat) {
        sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_DROP) == 0, PROFILE,
                           "playback repeated DROP: %s", strerror(errno));
        sound_test_require(pcm_state(fd) == SNDRV_PCM_STATE_SETUP, PROFILE,
                           "playback repeated DROP did not enter SETUP");
        sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_PREPARE) == 0, PROFILE,
                           "playback repeated PREPARE: %s", strerror(errno));
        sound_test_require(pcm_state(fd) == SNDRV_PCM_STATE_PREPARED, PROFILE,
                           "playback repeated PREPARE is not PREPARED");
    }
    sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_HW_FREE) == 0, PROFILE,
                       "playback HW_FREE: %s", strerror(errno));
    sound_test_require(pcm_state(fd) == SNDRV_PCM_STATE_OPEN, PROFILE,
                       "playback HW_FREE did not enter OPEN");

    free(samples);
    close(fd);
    int reopened = reopen_pcm(PLAYBACK_PATH);
    close(reopened);
}

static void check_capture(void)
{
    int fd = open(CAPTURE_PATH, O_RDWR | O_NONBLOCK | O_CLOEXEC);
    sound_test_require(fd >= 0, PROFILE, "open %s: %s", CAPTURE_PATH,
                       strerror(errno));
    check_duplicate_open(CAPTURE_PATH);
    check_mmap_and_sync_ptr(fd, EBADFD);

    struct pcm_config config = configure_pcm(fd, SNDRV_PCM_STREAM_CAPTURE);
    sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_PREPARE) == 0, PROFILE,
                       "capture PREPARE: %s", strerror(errno));
    check_dup2_dup3_close_range(fd);
    check_fork_inherits_fd(fd);
    check_mmap_and_sync_ptr(fd, ENXIO);

    size_t exact_frames =
        config.period_frames < 256u ? config.period_frames : 256u;
    size_t exact_bytes = exact_frames * 4u;
    size_t buffer_bytes = (size_t)config.buffer_frames * 4u;
    size_t sample_count =
        (size_t)config.buffer_frames * 2u * SOUND_TEST_CHANNELS;
    int16_t *samples = calloc(sample_count, sizeof(*samples));
    sound_test_require(samples != NULL, PROFILE, "allocate capture samples");

    sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_START) == 0, PROFILE,
                       "capture START: %s", strerror(errno));

    struct pollfd poll_fd = {.fd = fd, .events = POLLIN, .revents = 0};
    struct timespec timeout = {.tv_sec = 3, .tv_nsec = 0};
    sigset_t empty;
    sigemptyset(&empty);
    sound_test_require(ppoll(&poll_fd, 1, &timeout, &empty) == 1 &&
                           (poll_fd.revents & POLLIN) != 0,
                       PROFILE, "capture ppoll did not become readable");

    snd_pcm_sframes_t result =
        transfer(fd, SNDRV_PCM_IOCTL_READI_FRAMES, samples, exact_frames);
    sound_test_require(result == (snd_pcm_sframes_t)exact_frames, PROFILE,
                       "capture READI transferred %ld of %zu frames",
                       (long)result, exact_frames);
    sound_test_require(ppoll(&poll_fd, 1, &timeout, &empty) == 1 &&
                           (poll_fd.revents & POLLIN) != 0,
                       PROFILE, "capture byte-read ppoll did not become "
                                "readable");
    ssize_t count = read(fd, samples, exact_bytes);
    sound_test_require(count == (ssize_t)exact_bytes, PROFILE,
                       "capture read returned %zd of %zu bytes", count,
                       exact_bytes);

    sound_test_require(ppoll(&poll_fd, 1, &timeout, &empty) == 1 &&
                           (poll_fd.revents & POLLIN) != 0,
                       PROFILE, "capture partial ppoll did not become readable");
    count = read(fd, samples, buffer_bytes * 2u);
    sound_test_require(count > 0 && count % 4 == 0 &&
                           count < (ssize_t)(buffer_bytes * 2u),
                       PROFILE, "capture partial read returned %zd bytes",
                       count);

    struct snd_pcm_status before_bad;
    struct snd_pcm_status after_bad;
    pcm_status(fd, &before_bad);
    errno = 0;
    count = read(fd, samples, 3);
    sound_test_require(count < 0 && errno == EINVAL, PROFILE,
                       "capture non-frame read returned %zd errno %d", count,
                       errno);
    pcm_status(fd, &after_bad);
    sound_test_require(after_bad.appl_ptr == before_bad.appl_ptr, PROFILE,
                       "capture non-frame read advanced appl_ptr");

    sound_test_require(ppoll(&poll_fd, 1, &timeout, &empty) == 1 &&
                           (poll_fd.revents & POLLIN) != 0,
                       PROFILE,
                       "capture EFAULT probe did not become readable");
    errno = 0;
    count = read(fd, (void *)(uintptr_t)1, 4);
    sound_test_require(count < 0 && errno == EFAULT, PROFILE,
                       "capture bad user buffer returned %zd errno %d", count,
                       errno);
    pcm_status(fd, &after_bad);
    sound_test_require(after_bad.appl_ptr == before_bad.appl_ptr, PROFILE,
                       "capture EFAULT advanced appl_ptr");

    int saw_read_eagain = 0;
    for (unsigned int attempt = 0; attempt < 1000; ++attempt) {
        errno = 0;
        count = read(fd, samples, exact_bytes);
        if (count < 0) {
            sound_test_require(errno == EAGAIN, PROFILE,
                               "capture drain read returned errno %d", errno);
            saw_read_eagain = 1;
            break;
        }
        sound_test_require(count > 0 && count % 4 == 0, PROFILE,
                           "capture drain read returned %zd bytes", count);
    }
    sound_test_require(saw_read_eagain, PROFILE,
                       "non-blocking capture never reported EAGAIN");

    int epoll_fd = epoll_create1(EPOLL_CLOEXEC);
    sound_test_require(epoll_fd >= 0, PROFILE, "epoll_create1: %s",
                       strerror(errno));
    struct epoll_event event = {.events = EPOLLIN, .data.fd = fd};
    sound_test_require(epoll_ctl(epoll_fd, EPOLL_CTL_ADD, fd, &event) == 0,
                       PROFILE, "epoll_ctl: %s", strerror(errno));
    sound_test_require(epoll_wait(epoll_fd, &event, 1, 3000) == 1 &&
                           (event.events & EPOLLIN) != 0,
                       PROFILE, "capture epoll did not become readable");
    close(epoll_fd);

    drain_nonblocking_eagain(fd);
    int capture_drain_state = pcm_state(fd);
    sound_test_require(capture_drain_state == SNDRV_PCM_STATE_DRAINING, PROFILE,
                       "capture non-block DRAIN state=%d, expected DRAINING",
                       capture_drain_state);
    int saw_draining = 0;
    int saw_unread_read = 0;
    int saw_setup = 0;
    for (unsigned int attempt = 0; attempt < 200; ++attempt) {
        struct snd_pcm_status status;

        pcm_status(fd, &status);
        if (status.state == SNDRV_PCM_STATE_DRAINING) {
            saw_draining = 1;
            if (status.avail > 0u) {
                snd_pcm_uframes_t frames =
                    status.avail < 256u ? status.avail : 256u;

                count = read(fd, samples, (size_t)frames * 4u);
                sound_test_require(count > 0 && count % 4 == 0, PROFILE,
                                   "capture draining read returned %zd bytes",
                                   count);
                saw_unread_read = 1;
            }
        } else if (status.state == SNDRV_PCM_STATE_SETUP) {
            saw_setup = 1;
            break;
        } else {
            sound_test_fail(PROFILE,
                            "capture DRAIN entered unexpected state %d",
                            status.state);
        }
        sleep_ms(10);
    }
    sound_test_require(saw_draining && saw_unread_read && saw_setup, PROFILE,
                       "capture DRAIN did not preserve unread frames until "
                       "SETUP (draining=%d read=%d setup=%d)",
                       saw_draining, saw_unread_read, saw_setup);
    drain_blocking_after_nonblock(fd);
    sound_test_require(pcm_state(fd) == SNDRV_PCM_STATE_SETUP, PROFILE,
                       "capture blocking DRAIN left unexpected state");

    sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_PREPARE) == 0, PROFILE,
                       "capture PREPARE after DRAIN: %s", strerror(errno));
    sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_START) == 0, PROFILE,
                       "capture START before overrun: %s", strerror(errno));
    sleep_ms(buffer_wait_ms(&config));
    int state_xrun = pcm_state(fd) == SNDRV_PCM_STATE_XRUN;
    errno = 0;
    count = read(fd, samples, exact_bytes);
    sound_test_require(count < 0 && errno == EPIPE, PROFILE,
                       "capture overrun state=%d read=%zd errno=%d", state_xrun,
                       count, errno);
    sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_PREPARE) == 0, PROFILE,
                       "capture XRUN PREPARE recovery: %s", strerror(errno));
    sound_test_require(pcm_state(fd) == SNDRV_PCM_STATE_PREPARED, PROFILE,
                       "capture did not recover to PREPARED");

    for (unsigned int repeat = 0; repeat < 2; ++repeat) {
        sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_DROP) == 0, PROFILE,
                           "capture repeated DROP: %s", strerror(errno));
        sound_test_require(pcm_state(fd) == SNDRV_PCM_STATE_SETUP, PROFILE,
                           "capture repeated DROP did not enter SETUP");
        sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_PREPARE) == 0, PROFILE,
                           "capture repeated PREPARE: %s", strerror(errno));
        sound_test_require(pcm_state(fd) == SNDRV_PCM_STATE_PREPARED, PROFILE,
                           "capture repeated PREPARE is not PREPARED");
    }
    sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_HW_FREE) == 0, PROFILE,
                       "capture HW_FREE: %s", strerror(errno));
    sound_test_require(pcm_state(fd) == SNDRV_PCM_STATE_OPEN, PROFILE,
                       "capture HW_FREE did not enter OPEN");

    free(samples);
    close(fd);
    int reopened = reopen_pcm(CAPTURE_PATH);
    close(reopened);
}

static void check_blocking_read_drop_lifecycle(void)
{
    int fd = open(CAPTURE_PATH, O_RDWR | O_NONBLOCK | O_CLOEXEC);

    sound_test_require(fd >= 0, PROFILE, "open %s: %s", CAPTURE_PATH,
                       strerror(errno));
    struct pcm_config config = configure_pcm(fd, SNDRV_PCM_STREAM_CAPTURE);
    check_capture_blocking_read_drop(fd, &config);
    sound_test_require(ioctl(fd, SNDRV_PCM_IOCTL_HW_FREE) == 0, PROFILE,
                       "blocking-read HW_FREE: %s", strerror(errno));
    sound_test_require(pcm_state(fd) == SNDRV_PCM_STATE_OPEN, PROFILE,
                       "blocking-read HW_FREE did not enter OPEN");
    close(fd);

    int reopened = reopen_pcm(CAPTURE_PATH);
    close(reopened);
}

static void child_capture_loop(int ready_fd)
{
    int fd = reopen_pcm(CAPTURE_PATH);
    (void)configure_pcm(fd, SNDRV_PCM_STREAM_CAPTURE);
    if (ioctl(fd, SNDRV_PCM_IOCTL_PREPARE) < 0 ||
        ioctl(fd, SNDRV_PCM_IOCTL_START) < 0) {
        sound_test_fail(PROFILE, "child capture start: %s", strerror(errno));
    }
    char ready = 'R';
    if (write(ready_fd, &ready, sizeof(ready)) != sizeof(ready)) {
        _exit(2);
    }

    int16_t samples[256 * SOUND_TEST_CHANNELS];
    for (;;) {
        ssize_t count = read(fd, samples, sizeof(samples));

        if (count < 0 && errno == EINTR) {
            continue;
        }
        if (count < 0 && errno == EAGAIN) {
            sleep_ms(10);
            continue;
        }
        if (count < 0) {
            _exit(3);
        }
    }
}

static void check_killed_reader_reopen(void)
{
    int pipe_fds[2];
    sound_test_require(pipe(pipe_fds) == 0, PROFILE, "pipe: %s",
                       strerror(errno));
    pid_t child = fork();
    sound_test_require(child >= 0, PROFILE, "fork: %s", strerror(errno));
    if (child == 0) {
        close(pipe_fds[0]);
        child_capture_loop(pipe_fds[1]);
        _exit(4);
    }

    close(pipe_fds[1]);
    struct pollfd ready = {.fd = pipe_fds[0], .events = POLLIN, .revents = 0};
    sound_test_require(poll(&ready, 1, 3000) == 1 &&
                           (ready.revents & POLLIN) != 0,
                       PROFILE, "child capture did not become ready");
    char byte;
    sound_test_require(read(pipe_fds[0], &byte, sizeof(byte)) ==
                           sizeof(byte),
                       PROFILE, "child capture handshake");
    close(pipe_fds[0]);

    sleep_ms(100);
    sound_test_require(kill(child, SIGKILL) == 0, PROFILE,
                       "kill blocked capture reader: %s", strerror(errno));
    int status;
    sound_test_require(waitpid(child, &status, 0) == child, PROFILE,
                       "reap blocked capture reader");

    int reopened = reopen_pcm(CAPTURE_PATH);
    close(reopened);
}

int main(void)
{
    check_control();
    /* Keep the shared-OFD lock proof ahead of playback's OPEN DRAIN path. */
    check_blocking_read_drop_lifecycle();
    check_playback();
    check_capture();
    check_killed_reader_reopen();

    puts("VIRTIO_SOUND_HEADLESS_PASSED");
    fflush(stdout);
    return 0;
}
