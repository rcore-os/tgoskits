#include "uvc_capture.h"

#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <algorithm>

#include "turbojpeg.h"

const char *uvc_error(UvcApi *api, int err)
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

bool load_uvc_api(UvcApi *api)
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

void close_uvc_api(UvcApi *api)
{
    if (api->lib != NULL) {
        dlclose(api->lib);
        api->lib = NULL;
    }
}

static bool looks_like_mjpeg(const unsigned char *data, size_t size)
{
    return size >= 4 && data[0] == 0xff && data[1] == 0xd8 &&
           data[size - 2] == 0xff && data[size - 1] == 0xd9;
}

static void frame_callback(UvcFrame *frame, void *ptr)
{
    if (frame == NULL || ptr == NULL || frame->data == NULL || frame->data_bytes == 0) {
        return;
    }

    const unsigned char *data = reinterpret_cast<const unsigned char *>(frame->data);
    const size_t size = frame->data_bytes;
    int stored_format = frame->frame_format;
    if (looks_like_mjpeg(data, size)) {
        stored_format = UVC_FRAME_FORMAT_MJPEG;
    } else if (frame->frame_format != UVC_FRAME_FORMAT_YUYV) {
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
    state->latest.format = stored_format;
    state->latest.data.assign(data, data + size);
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
    const size_t min_size = (size_t)width * (size_t)height * 2;
    if (width <= 0 || height <= 0 || frame.data.size() < min_size) {
        printf("invalid YUYV frame size=%zu expected_at_least=%zu width=%d height=%d\n",
               frame.data.size(),
               min_size,
               width,
               height);
        return -1;
    }

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

int frame_to_image(const LatestFrame &frame, image_buffer_t *image)
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

bool snapshot_latest_capture(SharedState *state, LatestFrame *frame)
{
    std::lock_guard<std::mutex> guard(state->mutex);
    if (state->latest.id == 0) {
        return false;
    }
    *frame = state->latest;
    return true;
}

UvcCaptureCounters capture_counters(SharedState *state)
{
    std::lock_guard<std::mutex> guard(state->mutex);
    UvcCaptureCounters counters;
    counters.captured = state->captured;
    counters.bytes = state->bytes;
    counters.dropped = state->dropped;
    return counters;
}

static uvc_device *select_device(UvcApi *api, uvc_context *ctx, int index, const char *log_prefix)
{
    uvc_device **list = NULL;
    int ret = api->get_device_list(ctx, &list);
    if (ret < 0) {
        printf("uvc_get_device_list failed: %s\n", uvc_error(api, ret));
        return NULL;
    }

    uvc_device *selected = NULL;
    for (int i = 0; list != NULL && list[i] != NULL; ++i) {
        printf("%s: device index=%d bus=%u address=%u\n",
               log_prefix,
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

bool start_uvc_capture(UvcCaptureSession *session, const UvcCaptureOptions *options)
{
    const char *log_prefix = options->log_prefix != NULL ? options->log_prefix : "uvc";
    if (!load_uvc_api(&session->api)) {
        close_uvc_api(&session->api);
        return false;
    }

    int ret = session->api.init(&session->ctx, NULL);
    if (ret < 0) {
        printf("uvc_init failed: %s\n", uvc_error(&session->api, ret));
        close_uvc_api(&session->api);
        return false;
    }

    session->dev = select_device(&session->api, session->ctx, options->device, log_prefix);
    if (session->dev == NULL) {
        stop_uvc_capture(session);
        return false;
    }

    ret = session->api.open(session->dev, &session->devh);
    if (ret < 0) {
        printf("uvc_open failed: %s\n", uvc_error(&session->api, ret));
        stop_uvc_capture(session);
        return false;
    }

    memset(&session->ctrl, 0, sizeof(session->ctrl));
    ret = session->api.get_stream_ctrl_format_size(
        session->devh,
        &session->ctrl,
        UVC_FRAME_FORMAT_MJPEG,
        options->width,
        options->height,
        options->fps);
    if (ret < 0) {
        printf("uvc_get_stream_ctrl_format_size MJPEG failed: %s\n", uvc_error(&session->api, ret));
        ret = session->api.get_stream_ctrl_format_size(
            session->devh,
            &session->ctrl,
            UVC_FRAME_FORMAT_YUYV,
            options->width,
            options->height,
            options->fps);
    }
    if (ret < 0) {
        printf("uvc_get_stream_ctrl_format_size YUYV failed: %s\n", uvc_error(&session->api, ret));
        stop_uvc_capture(session);
        return false;
    }
    printf("%s: ctrl format_index=%u frame_index=%u interval=%u max_frame=%u max_payload=%u iface=%u\n",
           log_prefix,
           session->ctrl.b_format_index,
           session->ctrl.b_frame_index,
           session->ctrl.dw_frame_interval,
           session->ctrl.dw_max_video_frame_size,
           session->ctrl.dw_max_payload_transfer_size,
           session->ctrl.b_interface_number);

    ret = session->api.start_streaming(session->devh, &session->ctrl, frame_callback, &session->state, 0);
    if (ret < 0) {
        printf("uvc_start_streaming failed: %s\n", uvc_error(&session->api, ret));
        stop_uvc_capture(session);
        return false;
    }
    session->streaming = true;
    printf("%s: streaming started\n", log_prefix);
    return true;
}

void stop_uvc_capture(UvcCaptureSession *session)
{
    if (session->streaming && session->devh != NULL) {
        session->api.stop_streaming(session->devh);
        session->streaming = false;
    }
    if (session->devh != NULL) {
        session->api.close(session->devh);
        session->devh = NULL;
    }
    if (session->dev != NULL) {
        session->api.unref_device(session->dev);
        session->dev = NULL;
    }
    if (session->ctx != NULL) {
        session->api.exit(session->ctx);
        session->ctx = NULL;
    }
    close_uvc_api(&session->api);
}
