#ifndef RKNN_YOLOV8_UVC_CAPTURE_H_
#define RKNN_YOLOV8_UVC_CAPTURE_H_

#include <stddef.h>
#include <stdint.h>
#include <sys/time.h>
#include <time.h>

#include <mutex>
#include <vector>

#include "common.h"

static const int UVC_FRAME_FORMAT_ANY = 0;
static const int UVC_FRAME_FORMAT_YUYV = 3;
static const int UVC_FRAME_FORMAT_MJPEG = 7;

struct uvc_context;
struct uvc_device;
struct uvc_device_handle;

struct UvcFrame {
    void *data;
    size_t data_bytes;
    uint32_t width;
    uint32_t height;
    int frame_format;
    size_t step;
    uint32_t sequence;
    struct timeval capture_time;
    struct timespec capture_time_finished;
    uvc_device_handle *source;
    uint8_t library_owns_data;
    void *metadata;
    size_t metadata_bytes;
};

struct UvcStreamCtrl {
    uint16_t bm_hint;
    uint8_t b_format_index;
    uint8_t b_frame_index;
    uint32_t dw_frame_interval;
    uint16_t w_key_frame_rate;
    uint16_t w_p_frame_rate;
    uint16_t w_comp_quality;
    uint16_t w_comp_window_size;
    uint16_t w_delay;
    uint32_t dw_max_video_frame_size;
    uint32_t dw_max_payload_transfer_size;
    uint32_t dw_clock_frequency;
    uint8_t bm_framing_info;
    uint8_t b_preferred_version;
    uint8_t b_min_version;
    uint8_t b_max_version;
    uint8_t b_interface_number;
};

using UvcCallback = void (*)(UvcFrame *, void *);

struct UvcApi {
    void *lib = NULL;
    int (*init)(uvc_context **, void *) = NULL;
    void (*exit)(uvc_context *) = NULL;
    int (*get_device_list)(uvc_context *, uvc_device ***) = NULL;
    void (*free_device_list)(uvc_device **, uint8_t) = NULL;
    void (*ref_device)(uvc_device *) = NULL;
    void (*unref_device)(uvc_device *) = NULL;
    uint8_t (*get_bus_number)(uvc_device *) = NULL;
    uint8_t (*get_device_address)(uvc_device *) = NULL;
    int (*open)(uvc_device *, uvc_device_handle **) = NULL;
    void (*close)(uvc_device_handle *) = NULL;
    int (*get_stream_ctrl_format_size)(uvc_device_handle *, UvcStreamCtrl *, int, int, int, int) = NULL;
    int (*start_streaming)(uvc_device_handle *, UvcStreamCtrl *, UvcCallback, void *, uint8_t) = NULL;
    void (*stop_streaming)(uvc_device_handle *) = NULL;
    const char *(*strerror)(int) = NULL;
};

struct LatestFrame {
    uint64_t id = 0;
    uint32_t sequence = 0;
    int width = 0;
    int height = 0;
    int format = 0;
    std::vector<unsigned char> data;
};

struct SharedState {
    std::mutex mutex;
    LatestFrame latest;
    uint64_t captured = 0;
    uint64_t bytes = 0;
    uint64_t dropped = 0;
};

struct UvcCaptureOptions {
    int device = 0;
    int width = 320;
    int height = 240;
    int fps = 30;
    const char *log_prefix = "uvc";
};

struct UvcCaptureSession {
    UvcApi api;
    uvc_context *ctx = NULL;
    uvc_device *dev = NULL;
    uvc_device_handle *devh = NULL;
    UvcStreamCtrl ctrl;
    SharedState state;
    bool streaming = false;
};

struct UvcCaptureCounters {
    uint64_t captured = 0;
    uint64_t bytes = 0;
    uint64_t dropped = 0;
};

const char *uvc_error(UvcApi *api, int err);
bool load_uvc_api(UvcApi *api);
void close_uvc_api(UvcApi *api);

bool start_uvc_capture(UvcCaptureSession *session, const UvcCaptureOptions *options);
void stop_uvc_capture(UvcCaptureSession *session);
bool snapshot_latest_capture(SharedState *state, LatestFrame *frame);
UvcCaptureCounters capture_counters(SharedState *state);
int frame_to_image(const LatestFrame &frame, image_buffer_t *image);

#endif
