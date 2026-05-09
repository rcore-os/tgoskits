// Copyright (c) 2023 by Rockchip Electronics Co., Ltd. All Rights Reserved.
//
// Licensed under the Apache License, Version 2.0.

#include <dlfcn.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

#include <algorithm>
#include <mutex>
#include <string>
#include <vector>

#include "turbojpeg.h"
#include "yolov8.h"

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

struct Options {
    int device = 0;
    int width = 320;
    int height = 240;
    int fps = 30;
    int duration_sec = 10;
    int infer_every = 30;
    int max_inferences = 0;
    const char *model_path = "model/yolov8.rknn";
    const char *label_path = "model/coco_80_labels_list.txt";
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

static volatile sig_atomic_t g_running = 1;

static void signal_handler(int)
{
    g_running = 0;
}

static double monotonic_sec()
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec + (double)ts.tv_nsec / 1000000000.0;
}

static const char *uvc_error(UvcApi *api, int err)
{
    if (api->strerror != NULL) {
        const char *msg = api->strerror(err);
        if (msg != NULL) {
            return msg;
        }
    }
    return "unknown libuvc error";
}

static bool load_symbol(void *lib, const char *name, void **out)
{
    *out = dlsym(lib, name);
    if (*out == NULL) {
        printf("dlsym %s failed: %s\n", name, dlerror());
        return false;
    }
    return true;
}

#define LOAD_UVC_SYMBOL(api, field, symbol) \
    load_symbol((api)->lib, symbol, reinterpret_cast<void **>(&((api)->field)))

static bool load_uvc_api(UvcApi *api)
{
    const char *candidates[] = {"libuvc.so", "/usr/local/lib/libuvc.so", "/usr/lib/aarch64-linux-gnu/libuvc.so", NULL};
    for (int i = 0; candidates[i] != NULL && api->lib == NULL; ++i) {
        api->lib = dlopen(candidates[i], RTLD_NOW | RTLD_LOCAL);
    }
    if (api->lib == NULL) {
        printf("dlopen libuvc.so failed: %s\n", dlerror());
        return false;
    }

    return LOAD_UVC_SYMBOL(api, init, "uvc_init") &&
           LOAD_UVC_SYMBOL(api, exit, "uvc_exit") &&
           LOAD_UVC_SYMBOL(api, get_device_list, "uvc_get_device_list") &&
           LOAD_UVC_SYMBOL(api, free_device_list, "uvc_free_device_list") &&
           LOAD_UVC_SYMBOL(api, ref_device, "uvc_ref_device") &&
           LOAD_UVC_SYMBOL(api, unref_device, "uvc_unref_device") &&
           LOAD_UVC_SYMBOL(api, get_bus_number, "uvc_get_bus_number") &&
           LOAD_UVC_SYMBOL(api, get_device_address, "uvc_get_device_address") &&
           LOAD_UVC_SYMBOL(api, open, "uvc_open") &&
           LOAD_UVC_SYMBOL(api, close, "uvc_close") &&
           LOAD_UVC_SYMBOL(api, get_stream_ctrl_format_size, "uvc_get_stream_ctrl_format_size") &&
           LOAD_UVC_SYMBOL(api, start_streaming, "uvc_start_streaming") &&
           LOAD_UVC_SYMBOL(api, stop_streaming, "uvc_stop_streaming") &&
           LOAD_UVC_SYMBOL(api, strerror, "uvc_strerror");
}

static void close_uvc_api(UvcApi *api)
{
    if (api->lib != NULL) {
        dlclose(api->lib);
        api->lib = NULL;
    }
}

static void frame_callback(UvcFrame *frame, void *ptr)
{
    if (!g_running || frame == NULL || ptr == NULL || frame->data == NULL || frame->data_bytes == 0) {
        return;
    }

    SharedState *state = reinterpret_cast<SharedState *>(ptr);
    std::lock_guard<std::mutex> guard(state->mutex);
    state->captured++;
    state->bytes += frame->data_bytes;
    if (!state->latest.data.empty() && state->latest.id != state->captured) {
        state->dropped++;
    }
    state->latest.id = state->captured;
    state->latest.sequence = frame->sequence;
    state->latest.width = frame->width;
    state->latest.height = frame->height;
    state->latest.format = frame->frame_format;
    state->latest.data.assign(
        reinterpret_cast<unsigned char *>(frame->data),
        reinterpret_cast<unsigned char *>(frame->data) + frame->data_bytes);
}

static int decode_mjpeg(const LatestFrame &frame, image_buffer_t *image)
{
    tjhandle handle = tjInitDecompress();
    if (handle == NULL) {
        printf("tjInitDecompress failed\n");
        return -1;
    }

    int width = 0;
    int height = 0;
    int subsample = 0;
    int colorspace = 0;
    int ret = tjDecompressHeader3(
        handle,
        const_cast<unsigned char *>(frame.data.data()),
        (unsigned long)frame.data.size(),
        &width,
        &height,
        &subsample,
        &colorspace);
    if (ret != 0) {
        printf("tjDecompressHeader3 failed: %s\n", tjGetErrorStr());
        tjDestroy(handle);
        return -1;
    }

    const int size = width * height * 3;
    unsigned char *buf = reinterpret_cast<unsigned char *>(malloc(size));
    if (buf == NULL) {
        printf("malloc RGB buffer failed: size=%d\n", size);
        tjDestroy(handle);
        return -1;
    }

    ret = tjDecompress2(
        handle,
        const_cast<unsigned char *>(frame.data.data()),
        (unsigned long)frame.data.size(),
        buf,
        width,
        0,
        height,
        TJPF_RGB,
        0);
    if (ret != 0 && tjGetErrorCode(handle) != 0) {
        printf("tjDecompress2 failed: %s\n", tjGetErrorStr());
        free(buf);
        tjDestroy(handle);
        return -1;
    }

    tjDestroy(handle);
    memset(image, 0, sizeof(*image));
    image->width = width;
    image->height = height;
    image->width_stride = width * 3;
    image->height_stride = height;
    image->format = IMAGE_FORMAT_RGB888;
    image->virt_addr = buf;
    image->size = size;
    return 0;
}

static int copy_yuyv_as_rgb(const LatestFrame &frame, image_buffer_t *image)
{
    const int width = frame.width;
    const int height = frame.height;
    const int rgb_size = width * height * 3;
    unsigned char *rgb = reinterpret_cast<unsigned char *>(malloc(rgb_size));
    if (rgb == NULL) {
        printf("malloc YUYV RGB buffer failed: size=%d\n", rgb_size);
        return -1;
    }

    const unsigned char *src = frame.data.data();
    unsigned char *dst = rgb;
    for (int i = 0; i < width * height; i += 2) {
        int y0 = src[0];
        int u = src[1] - 128;
        int y1 = src[2];
        int v = src[3] - 128;
        int c0 = y0 - 16;
        int c1 = y1 - 16;
        int d = u;
        int e = v;
        int r0 = (298 * c0 + 409 * e + 128) >> 8;
        int g0 = (298 * c0 - 100 * d - 208 * e + 128) >> 8;
        int b0 = (298 * c0 + 516 * d + 128) >> 8;
        int r1 = (298 * c1 + 409 * e + 128) >> 8;
        int g1 = (298 * c1 - 100 * d - 208 * e + 128) >> 8;
        int b1 = (298 * c1 + 516 * d + 128) >> 8;
        dst[0] = (unsigned char)std::max(0, std::min(255, r0));
        dst[1] = (unsigned char)std::max(0, std::min(255, g0));
        dst[2] = (unsigned char)std::max(0, std::min(255, b0));
        dst[3] = (unsigned char)std::max(0, std::min(255, r1));
        dst[4] = (unsigned char)std::max(0, std::min(255, g1));
        dst[5] = (unsigned char)std::max(0, std::min(255, b1));
        src += 4;
        dst += 6;
    }

    memset(image, 0, sizeof(*image));
    image->width = width;
    image->height = height;
    image->width_stride = width * 3;
    image->height_stride = height;
    image->format = IMAGE_FORMAT_RGB888;
    image->virt_addr = rgb;
    image->size = rgb_size;
    return 0;
}

static int frame_to_image(const LatestFrame &frame, image_buffer_t *image)
{
    if (frame.format == UVC_FRAME_FORMAT_MJPEG) {
        return decode_mjpeg(frame, image);
    }
    if (frame.format == UVC_FRAME_FORMAT_YUYV) {
        return copy_yuyv_as_rgb(frame, image);
    }
    printf("unsupported frame format: %d\n", frame.format);
    return -1;
}

static void print_result(int inference_index, uint64_t frame_id, uint32_t sequence, double latency_ms, const object_detect_result_list *results)
{
    printf("YOLO_INFER index=%d frame=%llu sequence=%u latency_ms=%.2f detections=%d\n",
           inference_index,
           (unsigned long long)frame_id,
           sequence,
           latency_ms,
           results->count);

    if (results->count == 0) {
        printf("YOLO_RESULT index=%d frame=%llu detections=0\n",
               inference_index,
               (unsigned long long)frame_id);
        fflush(stdout);
        return;
    }

    for (int i = 0; i < results->count; ++i) {
        const object_detect_result *det = &results->results[i];
        int box_width = det->box.right - det->box.left;
        int box_height = det->box.bottom - det->box.top;
        int center_x = det->box.left + box_width / 2;
        int center_y = det->box.top + box_height / 2;
        printf("YOLO_RESULT index=%d det=%d class=%s confidence=%.1f%% box=(%d,%d,%d,%d) center=(%d,%d) size=%dx%d\n",
               inference_index,
               i,
               coco_cls_to_name(det->cls_id),
               det->prop * 100.0f,
               det->box.left,
               det->box.top,
               det->box.right,
               det->box.bottom,
               center_x,
               center_y,
               box_width,
               box_height);
    }
    fflush(stdout);
}

static void print_usage(const char *argv0)
{
    printf("Usage: %s [OPTIONS]\n", argv0);
    printf("  --model <PATH>          RKNN model [default: model/yolov8.rknn]\n");
    printf("  --label <PATH>          label file [default: model/coco_80_labels_list.txt]\n");
    printf("  --device <INDEX>        UVC device index [default: 0]\n");
    printf("  --width <PIXELS>        frame width [default: 320]\n");
    printf("  --height <PIXELS>       frame height [default: 240]\n");
    printf("  --fps <FPS>             camera FPS [default: 30]\n");
    printf("  --duration-sec <SECS>   run duration, 0 means forever [default: 10]\n");
    printf("  --infer-every <N>       infer every Nth captured frame [default: 30]\n");
    printf("  --max-inferences <N>    stop after N successful inferences\n");
}

static bool parse_int_arg(const char *name, const char *value, int *out)
{
    if (value[0] == '\0') {
        printf("invalid value for %s: %s\n", name, value);
        return false;
    }
    int parsed = 0;
    for (const char *p = value; *p != '\0'; ++p) {
        if (*p < '0' || *p > '9') {
            printf("invalid value for %s: %s\n", name, value);
            return false;
        }
        parsed = parsed * 10 + (*p - '0');
        if (parsed > 1000000) {
            printf("invalid value for %s: %s\n", name, value);
            return false;
        }
    }
    if (parsed <= 0) {
        printf("invalid value for %s: %s\n", name, value);
        return false;
    }
    *out = parsed;
    return true;
}

static bool parse_nonnegative_int_arg(const char *name, const char *value, int *out)
{
    if (value[0] == '\0') {
        printf("invalid value for %s: %s\n", name, value);
        return false;
    }
    int parsed = 0;
    for (const char *p = value; *p != '\0'; ++p) {
        if (*p < '0' || *p > '9') {
            printf("invalid value for %s: %s\n", name, value);
            return false;
        }
        parsed = parsed * 10 + (*p - '0');
        if (parsed > 1000000) {
            printf("invalid value for %s: %s\n", name, value);
            return false;
        }
    }
    *out = parsed;
    return true;
}

static bool parse_args(int argc, char **argv, Options *options)
{
    for (int i = 1; i < argc; ++i) {
        const char *arg = argv[i];
        const char *value = NULL;
        if (strcmp(arg, "-h") == 0 || strcmp(arg, "--help") == 0) {
            print_usage(argv[0]);
            exit(0);
        }
        if (i + 1 < argc) {
            value = argv[i + 1];
        }

        if (strcmp(arg, "--model") == 0 && value != NULL) {
            options->model_path = value;
            ++i;
        } else if (strcmp(arg, "--label") == 0 && value != NULL) {
            options->label_path = value;
            ++i;
        } else if (strcmp(arg, "--device") == 0 && value != NULL) {
            if (!parse_nonnegative_int_arg(arg, value, &options->device)) return false;
            ++i;
        } else if (strcmp(arg, "--width") == 0 && value != NULL) {
            if (!parse_int_arg(arg, value, &options->width)) return false;
            ++i;
        } else if (strcmp(arg, "--height") == 0 && value != NULL) {
            if (!parse_int_arg(arg, value, &options->height)) return false;
            ++i;
        } else if (strcmp(arg, "--fps") == 0 && value != NULL) {
            if (!parse_int_arg(arg, value, &options->fps)) return false;
            ++i;
        } else if (strcmp(arg, "--duration-sec") == 0 && value != NULL) {
            if (!parse_nonnegative_int_arg(arg, value, &options->duration_sec)) return false;
            ++i;
        } else if (strcmp(arg, "--infer-every") == 0 && value != NULL) {
            if (!parse_int_arg(arg, value, &options->infer_every)) return false;
            ++i;
        } else if (strcmp(arg, "--max-inferences") == 0 && value != NULL) {
            if (!parse_nonnegative_int_arg(arg, value, &options->max_inferences)) return false;
            ++i;
        } else {
            printf("unknown or incomplete argument: %s\n", arg);
            return false;
        }
    }
    return true;
}

static uvc_device *select_device(UvcApi *api, uvc_context *ctx, int index)
{
    uvc_device **list = NULL;
    int ret = api->get_device_list(ctx, &list);
    if (ret < 0) {
        printf("uvc_get_device_list failed: %s\n", uvc_error(api, ret));
        return NULL;
    }

    uvc_device *selected = NULL;
    for (int i = 0; list != NULL && list[i] != NULL; ++i) {
        printf("stream-rknn: device index=%d bus=%u address=%u\n",
               i,
               api->get_bus_number(list[i]),
               api->get_device_address(list[i]));
        if (i == index) {
            selected = list[i];
            api->ref_device(selected);
        }
    }
    api->free_device_list(list, 1);
    if (selected == NULL) {
        printf("no UVC device at index %d\n", index);
    }
    return selected;
}

int main(int argc, char **argv)
{
    Options options;
    if (!parse_args(argc, argv, &options)) {
        print_usage(argv[0]);
        return 2;
    }

    signal(SIGINT, signal_handler);
    signal(SIGTERM, signal_handler);

    printf("YOLOv8 UVC Streaming Detection\n");
    printf("===============================\n");
    printf("model=%s label=%s device=%d size=%dx%d fps=%d duration=%d infer_every=%d max_inferences=%d\n",
           options.model_path,
           options.label_path,
           options.device,
           options.width,
           options.height,
           options.fps,
           options.duration_sec,
           options.infer_every,
           options.max_inferences);

    int ret = init_post_process(options.label_path);
    if (ret != 0) {
        printf("init_post_process fail! ret=%d label_path=%s\n", ret, options.label_path);
        return 1;
    }

    rknn_app_context_t app_ctx;
    memset(&app_ctx, 0, sizeof(app_ctx));
    ret = init_yolov8_model(options.model_path, &app_ctx);
    if (ret != 0) {
        printf("init_yolov8_model fail! ret=%d model_path=%s\n", ret, options.model_path);
        deinit_post_process();
        return 1;
    }
    printf("stream-rknn: init_yolov8_model success width=%d height=%d channel=%d\n",
           app_ctx.model_width,
           app_ctx.model_height,
           app_ctx.model_channel);

    UvcApi api;
    if (!load_uvc_api(&api)) {
        release_yolov8_model(&app_ctx);
        deinit_post_process();
        return 1;
    }

    uvc_context *ctx = NULL;
    ret = api.init(&ctx, NULL);
    if (ret < 0) {
        printf("uvc_init failed: %s\n", uvc_error(&api, ret));
        close_uvc_api(&api);
        release_yolov8_model(&app_ctx);
        deinit_post_process();
        return 1;
    }

    uvc_device *dev = select_device(&api, ctx, options.device);
    if (dev == NULL) {
        api.exit(ctx);
        close_uvc_api(&api);
        release_yolov8_model(&app_ctx);
        deinit_post_process();
        return 1;
    }

    uvc_device_handle *devh = NULL;
    ret = api.open(dev, &devh);
    if (ret < 0) {
        printf("uvc_open failed: %s\n", uvc_error(&api, ret));
        api.unref_device(dev);
        api.exit(ctx);
        close_uvc_api(&api);
        release_yolov8_model(&app_ctx);
        deinit_post_process();
        return 1;
    }

    UvcStreamCtrl ctrl;
    memset(&ctrl, 0, sizeof(ctrl));
    ret = api.get_stream_ctrl_format_size(
        devh,
        &ctrl,
        UVC_FRAME_FORMAT_MJPEG,
        options.width,
        options.height,
        options.fps);
    if (ret < 0) {
        printf("uvc_get_stream_ctrl_format_size MJPEG failed: %s\n", uvc_error(&api, ret));
        ret = api.get_stream_ctrl_format_size(
            devh,
            &ctrl,
            UVC_FRAME_FORMAT_YUYV,
            options.width,
            options.height,
            options.fps);
    }
    if (ret < 0) {
        printf("uvc_get_stream_ctrl_format_size YUYV failed: %s\n", uvc_error(&api, ret));
        api.close(devh);
        api.unref_device(dev);
        api.exit(ctx);
        close_uvc_api(&api);
        release_yolov8_model(&app_ctx);
        deinit_post_process();
        return 1;
    }
    printf("stream-rknn: ctrl format_index=%u frame_index=%u interval=%u max_frame=%u max_payload=%u iface=%u\n",
           ctrl.b_format_index,
           ctrl.b_frame_index,
           ctrl.dw_frame_interval,
           ctrl.dw_max_video_frame_size,
           ctrl.dw_max_payload_transfer_size,
           ctrl.b_interface_number);

    SharedState state;
    ret = api.start_streaming(devh, &ctrl, frame_callback, &state, 0);
    if (ret < 0) {
        printf("uvc_start_streaming failed: %s\n", uvc_error(&api, ret));
        api.close(devh);
        api.unref_device(dev);
        api.exit(ctx);
        close_uvc_api(&api);
        release_yolov8_model(&app_ctx);
        deinit_post_process();
        return 1;
    }
    printf("stream-rknn: streaming started\n");

    const double start = monotonic_sec();
    double last_report = start;
    uint64_t last_report_captured = 0;
    uint64_t last_inferred_frame = 0;
    int inferences = 0;
    int decode_errors = 0;
    int inference_errors = 0;

    while (g_running && (options.duration_sec == 0 || monotonic_sec() - start < options.duration_sec)) {
        LatestFrame frame;
        {
            std::lock_guard<std::mutex> guard(state.mutex);
            if (state.latest.id != 0 && state.latest.id != last_inferred_frame &&
                (state.latest.id % (uint64_t)options.infer_every) == 0) {
                frame = state.latest;
            }
        }

        if (frame.id == 0) {
            usleep(10000);
        } else {
            image_buffer_t image;
            memset(&image, 0, sizeof(image));
            double decode_start = monotonic_sec();
            if (frame_to_image(frame, &image) != 0) {
                decode_errors++;
                last_inferred_frame = frame.id;
                continue;
            }
            object_detect_result_list results;
            memset(&results, 0, sizeof(results));
            double infer_start = monotonic_sec();
            ret = inference_yolov8_model(&app_ctx, &image, &results);
            double infer_end = monotonic_sec();
            free(image.virt_addr);
            last_inferred_frame = frame.id;
            if (ret != 0) {
                printf("inference_yolov8_model fail! ret=%d frame=%llu\n", ret, (unsigned long long)frame.id);
                inference_errors++;
            } else {
                inferences++;
                printf("stream-rknn: decode_ms=%.2f ", (infer_start - decode_start) * 1000.0);
                print_result(inferences, frame.id, frame.sequence, (infer_end - infer_start) * 1000.0, &results);
            }
            if (options.max_inferences > 0 && inferences >= options.max_inferences) {
                break;
            }
        }

        double now = monotonic_sec();
        if (now - last_report >= 1.0) {
            uint64_t captured = 0;
            uint64_t bytes = 0;
            uint64_t dropped = 0;
            {
                std::lock_guard<std::mutex> guard(state.mutex);
                captured = state.captured;
                bytes = state.bytes;
                dropped = state.dropped;
            }
            double interval = now - last_report;
            uint64_t delta = captured - last_report_captured;
            printf("stream-rknn: capture_fps=%.2f captured=%llu inferred=%d dropped_latest=%llu mib_s=%.2f elapsed=%.1f\n",
                   (double)delta / interval,
                   (unsigned long long)captured,
                   inferences,
                   (unsigned long long)dropped,
                   (double)bytes / (now - start) / 1024.0 / 1024.0,
                   now - start);
            last_report = now;
            last_report_captured = captured;
        }
    }

    api.stop_streaming(devh);
    api.close(devh);
    api.unref_device(dev);
    api.exit(ctx);
    close_uvc_api(&api);

    double elapsed = monotonic_sec() - start;
    uint64_t captured = 0;
    uint64_t bytes = 0;
    uint64_t dropped = 0;
    {
        std::lock_guard<std::mutex> guard(state.mutex);
        captured = state.captured;
        bytes = state.bytes;
        dropped = state.dropped;
    }

    int release_ret = release_yolov8_model(&app_ctx);
    deinit_post_process();

    printf("stream-rknn: done duration_sec=%.1f captured=%llu capture_fps=%.2f inferences=%d infer_fps=%.2f dropped_latest=%llu decode_errors=%d inference_errors=%d bytes=%llu\n",
           elapsed,
           (unsigned long long)captured,
           (double)captured / std::max(elapsed, 0.001),
           inferences,
           (double)inferences / std::max(elapsed, 0.001),
           (unsigned long long)dropped,
           decode_errors,
           inference_errors,
           (unsigned long long)bytes);

    if (release_ret == 0 && inferences > 0 && inference_errors == 0) {
        printf("UVC_RKNN_STREAM_DONE\n");
        return 0;
    }
    return 1;
}
