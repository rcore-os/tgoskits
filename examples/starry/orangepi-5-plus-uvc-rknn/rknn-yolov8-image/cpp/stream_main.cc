// Copyright (c) 2023 by Rockchip Electronics Co., Ltd. All Rights Reserved.
//
// Licensed under the Apache License, Version 2.0.

#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <arpa/inet.h>
#include <netinet/in.h>
#include <pthread.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

#include <algorithm>
#include <chrono>
#include <condition_variable>
#include <mutex>
#include <string>
#include <vector>

#include "turbojpeg.h"
#include "image_drawing.h"
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
    int http_port = 0;
    int http_fps = 15;
    int jpeg_quality = 80;
    const char *push_host = NULL;
    int push_port = 18080;
    int push_fps = 15;
    bool draw_result = true;
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

struct HttpFrameState {
    std::mutex mutex;
    std::condition_variable updated;
    std::vector<unsigned char> jpeg;
    uint64_t frame_id = 0;
    int width = 0;
    int height = 0;
    int detections = 0;
    bool running = false;
};

struct HttpServer {
    int port = 0;
    int listen_fd = -1;
    int target_fps = 15;
    pthread_t thread = 0;
    HttpFrameState *frames = NULL;
};

struct HttpClient {
    int fd = -1;
    int target_fps = 15;
    HttpFrameState *frames = NULL;
};

struct PushClient {
    std::string host;
    int port = 0;
    int target_fps = 15;
    pthread_t thread = 0;
    HttpFrameState *frames = NULL;
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

static int clamp_int(int value, int low, int high)
{
    return std::max(low, std::min(high, value));
}

static void draw_detection_results(image_buffer_t *image, const object_detect_result_list *results)
{
    for (int i = 0; i < results->count; ++i) {
        const object_detect_result *det = &results->results[i];
        int left = clamp_int(det->box.left, 0, image->width - 1);
        int top = clamp_int(det->box.top, 0, image->height - 1);
        int right = clamp_int(det->box.right, 0, image->width - 1);
        int bottom = clamp_int(det->box.bottom, 0, image->height - 1);
        if (right <= left || bottom <= top) {
            continue;
        }

        draw_rectangle(image, left, top, right - left, bottom - top, COLOR_GREEN, 2);

        char label[96];
        snprintf(label, sizeof(label), "%s %.0f%%", coco_cls_to_name(det->cls_id), det->prop * 100.0f);
        int text_y = top > 18 ? top - 18 : top + 4;
        draw_text(image, label, left, text_y, COLOR_YELLOW, 10);
    }
}

static int encode_jpeg(const image_buffer_t *image, int quality, std::vector<unsigned char> *jpeg)
{
    tjhandle handle = tjInitCompress();
    if (handle == NULL) {
        printf("tjInitCompress failed\n");
        return -1;
    }

    unsigned char *jpeg_buf = NULL;
    unsigned long jpeg_size = 0;
    int ret = tjCompress2(
        handle,
        image->virt_addr,
        image->width,
        0,
        image->height,
        TJPF_RGB,
        &jpeg_buf,
        &jpeg_size,
        TJSAMP_420,
        quality,
        TJFLAG_FASTDCT);
    if (ret != 0) {
        printf("tjCompress2 failed: %s\n", tjGetErrorStr());
        tjDestroy(handle);
        return -1;
    }

    jpeg->assign(jpeg_buf, jpeg_buf + jpeg_size);
    tjFree(jpeg_buf);
    tjDestroy(handle);
    return 0;
}

static void publish_http_frame(HttpFrameState *state, uint64_t frame_id, int width, int height, int detections, const std::vector<unsigned char> &jpeg)
{
    std::lock_guard<std::mutex> guard(state->mutex);
    state->jpeg = jpeg;
    state->frame_id = frame_id;
    state->width = width;
    state->height = height;
    state->detections = detections;
    state->updated.notify_all();
}

static bool send_all(int fd, const void *data, size_t len)
{
    const unsigned char *ptr = reinterpret_cast<const unsigned char *>(data);
    while (len > 0) {
        ssize_t written = send(fd, ptr, len, 0);
        if (written < 0) {
            if (errno == EINTR) {
                continue;
            }
            return false;
        }
        if (written == 0) {
            return false;
        }
        ptr += written;
        len -= (size_t)written;
    }
    return true;
}

static bool send_text(int fd, const char *text)
{
    return send_all(fd, text, strlen(text));
}

static bool send_u32_be(int fd, uint32_t value)
{
    uint32_t net_value = htonl(value);
    return send_all(fd, &net_value, sizeof(net_value));
}

static void close_client(int fd)
{
    if (fd >= 0) {
        close(fd);
    }
}

static int connect_tcp_ipv4(const char *host, int port)
{
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) {
        return -1;
    }

    sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_port = htons((uint16_t)port);
    if (inet_pton(AF_INET, host, &addr.sin_addr) != 1) {
        close(fd);
        errno = EINVAL;
        return -1;
    }
    if (connect(fd, reinterpret_cast<sockaddr *>(&addr), sizeof(addr)) != 0) {
        close(fd);
        return -1;
    }
    return fd;
}

static void sleep_interruptible_ms(int ms)
{
    const int step_ms = 100;
    int slept = 0;
    while (g_running && slept < ms) {
        int chunk = std::min(step_ms, ms - slept);
        usleep((useconds_t)chunk * 1000);
        slept += chunk;
    }
}

static bool read_http_request(int fd, char *buf, size_t size)
{
    size_t used = 0;
    while (used + 1 < size) {
        ssize_t got = recv(fd, buf + used, size - used - 1, 0);
        if (got < 0) {
            if (errno == EINTR) {
                continue;
            }
            return false;
        }
        if (got == 0) {
            break;
        }
        used += (size_t)got;
        buf[used] = '\0';
        if (strstr(buf, "\r\n\r\n") != NULL || strstr(buf, "\n\n") != NULL) {
            return true;
        }
    }
    buf[used] = '\0';
    return used > 0;
}

static bool snapshot_frame(HttpFrameState *state, std::vector<unsigned char> *jpeg, uint64_t *frame_id, int *width, int *height, int *detections)
{
    std::lock_guard<std::mutex> guard(state->mutex);
    if (state->jpeg.empty()) {
        return false;
    }
    *jpeg = state->jpeg;
    *frame_id = state->frame_id;
    *width = state->width;
    *height = state->height;
    *detections = state->detections;
    return true;
}

static void handle_snapshot_client(int fd, HttpFrameState *state)
{
    std::vector<unsigned char> jpeg;
    uint64_t frame_id = 0;
    int width = 0;
    int height = 0;
    int detections = 0;
    if (!snapshot_frame(state, &jpeg, &frame_id, &width, &height, &detections)) {
        send_text(fd, "HTTP/1.1 503 Service Unavailable\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nno frame available\n");
        close_client(fd);
        return;
    }

    char header[512];
    snprintf(header, sizeof(header),
             "HTTP/1.1 200 OK\r\n"
             "Content-Type: image/jpeg\r\n"
             "Content-Length: %zu\r\n"
             "Cache-Control: no-cache\r\n"
             "X-Frame-Id: %llu\r\n"
             "X-Detections: %d\r\n"
             "Connection: close\r\n\r\n",
             jpeg.size(),
             (unsigned long long)frame_id,
             detections);
    send_text(fd, header);
    send_all(fd, jpeg.data(), jpeg.size());
    close_client(fd);
}

static void handle_stream_client(int fd, HttpFrameState *state, int target_fps)
{
    if (!send_text(fd,
                   "HTTP/1.1 200 OK\r\n"
                   "Content-Type: multipart/x-mixed-replace; boundary=frame\r\n"
                   "Cache-Control: no-cache\r\n"
                   "Pragma: no-cache\r\n"
                   "Connection: close\r\n\r\n")) {
        close_client(fd);
        return;
    }

    uint64_t last_frame = 0;
    const int sleep_us = target_fps > 0 ? 1000000 / target_fps : 100000;
    while (g_running && state->running) {
        std::vector<unsigned char> jpeg;
        uint64_t frame_id = 0;
        int width = 0;
        int height = 0;
        int detections = 0;
        {
            std::unique_lock<std::mutex> lock(state->mutex);
            state->updated.wait_for(lock, std::chrono::milliseconds(100), [&] {
                return !g_running || !state->running || (!state->jpeg.empty() && state->frame_id != last_frame);
            });
            if (!g_running || !state->running) {
                break;
            }
            if (state->jpeg.empty() || state->frame_id == last_frame) {
                continue;
            }
            jpeg = state->jpeg;
            frame_id = state->frame_id;
            width = state->width;
            height = state->height;
            detections = state->detections;
        }
        last_frame = frame_id;

        char part_header[512];
        snprintf(part_header, sizeof(part_header),
                 "--frame\r\n"
                 "Content-Type: image/jpeg\r\n"
                 "Content-Length: %zu\r\n"
                 "X-Frame-Id: %llu\r\n"
                 "X-Size: %dx%d\r\n"
                 "X-Detections: %d\r\n\r\n",
                 jpeg.size(),
                 (unsigned long long)frame_id,
                 width,
                 height,
                 detections);
        if (!send_text(fd, part_header) ||
            !send_all(fd, jpeg.data(), jpeg.size()) ||
            !send_text(fd, "\r\n")) {
            break;
        }
        if (sleep_us > 0) {
            usleep(sleep_us);
        }
    }
    close_client(fd);
}

static void *http_client_thread(void *arg)
{
    HttpClient *client = reinterpret_cast<HttpClient *>(arg);
    int fd = client->fd;
    HttpFrameState *frames = client->frames;
    int target_fps = client->target_fps;
    delete client;

    char request[2048];
    if (!read_http_request(fd, request, sizeof(request))) {
        close_client(fd);
        return NULL;
    }
    if (strncmp(request, "GET /stream.mjpg ", 17) == 0 ||
        strncmp(request, "GET / ", 6) == 0) {
        handle_stream_client(fd, frames, target_fps);
    } else if (strncmp(request, "GET /snapshot.jpg ", 18) == 0) {
        handle_snapshot_client(fd, frames);
    } else {
        send_text(fd,
                  "HTTP/1.1 200 OK\r\n"
                  "Content-Type: text/html\r\n"
                  "Connection: close\r\n\r\n"
                  "<!doctype html><html><head><title>RKNN YOLOv8 Stream</title></head>"
                  "<body style=\"margin:0;background:#111;color:#eee;font-family:sans-serif\">"
                  "<img src=\"/stream.mjpg\" style=\"max-width:100vw;max-height:100vh;display:block;margin:auto\">"
                  "</body></html>\n");
        close_client(fd);
    }
    return NULL;
}

static void *http_server_thread(void *arg)
{
    HttpServer *server = reinterpret_cast<HttpServer *>(arg);
    const int listen_fd = server->listen_fd;
    while (g_running && server->frames->running) {
        fd_set readfds;
        FD_ZERO(&readfds);
        FD_SET(listen_fd, &readfds);
        struct timeval timeout;
        timeout.tv_sec = 0;
        timeout.tv_usec = 200000;
        int ready = select(listen_fd + 1, &readfds, NULL, NULL, &timeout);
        if (ready < 0) {
            if (errno == EINTR) {
                continue;
            }
            printf("http: select failed: %s\n", strerror(errno));
            break;
        }
        if (ready == 0) {
            continue;
        }

        int fd = accept(listen_fd, NULL, NULL);
        if (fd < 0) {
            if (!g_running || !server->frames->running) {
                break;
            }
            if (errno == EINTR) {
                continue;
            }
            printf("http: accept failed: %s\n", strerror(errno));
            continue;
        }

        HttpClient *client = new HttpClient;
        if (client == NULL) {
            close_client(fd);
            continue;
        }
        client->fd = fd;
        client->target_fps = server->target_fps;
        client->frames = server->frames;
        pthread_t thread;
        if (pthread_create(&thread, NULL, http_client_thread, client) != 0) {
            delete client;
            close_client(fd);
            continue;
        }
        pthread_detach(thread);
    }
    return NULL;
}

static bool start_http_server(HttpServer *server, HttpFrameState *frames, int port, int target_fps)
{
    server->port = port;
    server->target_fps = target_fps;
    server->frames = frames;
    server->listen_fd = socket(AF_INET, SOCK_STREAM, 0);
    if (server->listen_fd < 0) {
        printf("http: socket failed: %s\n", strerror(errno));
        return false;
    }

    int one = 1;
    setsockopt(server->listen_fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));

    sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = htonl(INADDR_ANY);
    addr.sin_port = htons((uint16_t)port);
    if (bind(server->listen_fd, reinterpret_cast<sockaddr *>(&addr), sizeof(addr)) != 0) {
        printf("http: bind 0.0.0.0:%d failed: %s\n", port, strerror(errno));
        close(server->listen_fd);
        server->listen_fd = -1;
        return false;
    }
    if (listen(server->listen_fd, 8) != 0) {
        printf("http: listen failed: %s\n", strerror(errno));
        close(server->listen_fd);
        server->listen_fd = -1;
        return false;
    }

    frames->running = true;
    if (pthread_create(&server->thread, NULL, http_server_thread, server) != 0) {
        printf("http: pthread_create failed\n");
        close(server->listen_fd);
        server->listen_fd = -1;
        return false;
    }

    printf("stream-rknn: HTTP MJPEG server listening on 0.0.0.0:%d\n", port);
    printf("stream-rknn: open http://<board-ip>:%d/stream.mjpg or /snapshot.jpg\n", port);
    return true;
}

static void stop_http_server(HttpServer *server, HttpFrameState *frames)
{
    {
        std::lock_guard<std::mutex> guard(frames->mutex);
        frames->running = false;
        frames->updated.notify_all();
    }
    if (server->listen_fd >= 0) {
        shutdown(server->listen_fd, SHUT_RDWR);
        close(server->listen_fd);
    }
    if (server->thread != 0) {
        pthread_join(server->thread, NULL);
        server->thread = 0;
    }
    server->listen_fd = -1;
}

static bool wait_next_frame(HttpFrameState *state, uint64_t last_frame, std::vector<unsigned char> *jpeg, uint64_t *frame_id)
{
    std::unique_lock<std::mutex> lock(state->mutex);
    state->updated.wait_for(lock, std::chrono::milliseconds(100), [&] {
        return !g_running || !state->running || (!state->jpeg.empty() && state->frame_id != last_frame);
    });
    if (!g_running || !state->running || state->jpeg.empty() || state->frame_id == last_frame) {
        return false;
    }
    *jpeg = state->jpeg;
    *frame_id = state->frame_id;
    return true;
}

static void *push_client_thread(void *arg)
{
    PushClient *client = reinterpret_cast<PushClient *>(arg);
    HttpFrameState *frames = client->frames;
    const int sleep_us = client->target_fps > 0 ? 1000000 / client->target_fps : 100000;
    double last_error_log = 0.0;

    while (g_running && frames->running) {
        int fd = connect_tcp_ipv4(client->host.c_str(), client->port);
        if (fd < 0) {
            double now = monotonic_sec();
            if (now - last_error_log >= 5.0) {
                printf("push: connect %s:%d failed: %s; retrying\n", client->host.c_str(), client->port, strerror(errno));
                fflush(stdout);
                last_error_log = now;
            }
            sleep_interruptible_ms(1000);
            continue;
        }

        printf("push: connected to relay %s:%d\n", client->host.c_str(), client->port);
        fflush(stdout);
        if (!send_text(fd, "SRKNMJPG1\n")) {
            close_client(fd);
            continue;
        }

        uint64_t last_frame = 0;
        while (g_running && frames->running) {
            std::vector<unsigned char> jpeg;
            uint64_t frame_id = 0;
            if (!wait_next_frame(frames, last_frame, &jpeg, &frame_id)) {
                continue;
            }
            last_frame = frame_id;
            if (jpeg.size() > 64U * 1024U * 1024U ||
                !send_u32_be(fd, (uint32_t)jpeg.size()) ||
                !send_all(fd, jpeg.data(), jpeg.size())) {
                printf("push: relay disconnected; reconnecting\n");
                fflush(stdout);
                break;
            }
            if (sleep_us > 0) {
                usleep(sleep_us);
            }
        }
        close_client(fd);
    }
    return NULL;
}

static bool start_push_client(PushClient *client, HttpFrameState *frames, const char *host, int port, int target_fps)
{
    client->host = host;
    client->port = port;
    client->target_fps = target_fps;
    client->frames = frames;
    if (pthread_create(&client->thread, NULL, push_client_thread, client) != 0) {
        printf("push: pthread_create failed\n");
        client->thread = 0;
        return false;
    }
    printf("stream-rknn: pushing MJPEG frames to %s:%d\n", host, port);
    return true;
}

static void stop_push_client(PushClient *client, HttpFrameState *frames)
{
    {
        std::lock_guard<std::mutex> guard(frames->mutex);
        frames->running = false;
        frames->updated.notify_all();
    }
    if (client->thread != 0) {
        pthread_join(client->thread, NULL);
        client->thread = 0;
    }
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
    printf("  --http-port <PORT>      serve annotated MJPEG stream, 0 disables [default: 0]\n");
    printf("  --http-fps <FPS>        max MJPEG response FPS [default: 15]\n");
    printf("  --push-host <IP>        push annotated JPEG frames to relay host\n");
    printf("  --push-port <PORT>      relay ingest TCP port [default: 18080]\n");
    printf("  --push-fps <FPS>        max relay push FPS [default: 15]\n");
    printf("  --jpeg-quality <1-100>  MJPEG JPEG quality [default: 80]\n");
    printf("  --no-draw-result        publish raw frames without result boxes/text\n");
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
        } else if (strcmp(arg, "--http-port") == 0 && value != NULL) {
            if (!parse_nonnegative_int_arg(arg, value, &options->http_port)) return false;
            if (options->http_port > 65535) {
                printf("invalid value for %s: %s\n", arg, value);
                return false;
            }
            ++i;
        } else if (strcmp(arg, "--http-fps") == 0 && value != NULL) {
            if (!parse_int_arg(arg, value, &options->http_fps)) return false;
            ++i;
        } else if (strcmp(arg, "--push-host") == 0 && value != NULL) {
            options->push_host = value;
            ++i;
        } else if (strcmp(arg, "--push-port") == 0 && value != NULL) {
            if (!parse_int_arg(arg, value, &options->push_port)) return false;
            if (options->push_port > 65535) {
                printf("invalid value for %s: %s\n", arg, value);
                return false;
            }
            ++i;
        } else if (strcmp(arg, "--push-fps") == 0 && value != NULL) {
            if (!parse_int_arg(arg, value, &options->push_fps)) return false;
            ++i;
        } else if (strcmp(arg, "--jpeg-quality") == 0 && value != NULL) {
            if (!parse_int_arg(arg, value, &options->jpeg_quality)) return false;
            if (options->jpeg_quality < 1 || options->jpeg_quality > 100) {
                printf("invalid value for %s: %s\n", arg, value);
                return false;
            }
            ++i;
        } else if (strcmp(arg, "--no-draw-result") == 0) {
            options->draw_result = false;
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
    signal(SIGPIPE, SIG_IGN);

    printf("YOLOv8 UVC Streaming Detection\n");
    printf("===============================\n");
    printf("model=%s label=%s device=%d size=%dx%d fps=%d duration=%d infer_every=%d max_inferences=%d http_port=%d http_fps=%d push_host=%s push_port=%d push_fps=%d jpeg_quality=%d draw_result=%d\n",
           options.model_path,
           options.label_path,
           options.device,
           options.width,
           options.height,
           options.fps,
           options.duration_sec,
           options.infer_every,
           options.max_inferences,
           options.http_port,
           options.http_fps,
           options.push_host != NULL ? options.push_host : "",
           options.push_port,
           options.push_fps,
           options.jpeg_quality,
           options.draw_result ? 1 : 0);

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

    HttpFrameState http_frames;
    HttpServer http_server;
    bool http_started = false;
    PushClient push_client;
    bool push_started = false;
    if (options.http_port > 0 || options.push_host != NULL) {
        std::lock_guard<std::mutex> guard(http_frames.mutex);
        http_frames.running = true;
    }
    if (options.http_port > 0) {
        http_started = start_http_server(&http_server, &http_frames, options.http_port, options.http_fps);
        if (!http_started) {
            api.stop_streaming(devh);
            api.close(devh);
            api.unref_device(dev);
            api.exit(ctx);
            close_uvc_api(&api);
            release_yolov8_model(&app_ctx);
            deinit_post_process();
            return 1;
        }
    }
    if (options.push_host != NULL) {
        push_started = start_push_client(&push_client, &http_frames, options.push_host, options.push_port, options.push_fps);
        if (!push_started) {
            if (http_started) {
                stop_http_server(&http_server, &http_frames);
            }
            api.stop_streaming(devh);
            api.close(devh);
            api.unref_device(dev);
            api.exit(ctx);
            close_uvc_api(&api);
            release_yolov8_model(&app_ctx);
            deinit_post_process();
            return 1;
        }
    }

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
            last_inferred_frame = frame.id;
            if (ret != 0) {
                printf("inference_yolov8_model fail! ret=%d frame=%llu\n", ret, (unsigned long long)frame.id);
                inference_errors++;
            } else {
                inferences++;
                if (http_started || push_started) {
                    if (options.draw_result) {
                        draw_detection_results(&image, &results);
                    }
                    std::vector<unsigned char> jpeg;
                    if (encode_jpeg(&image, options.jpeg_quality, &jpeg) == 0) {
                        publish_http_frame(&http_frames, frame.id, image.width, image.height, results.count, jpeg);
                    }
                }
                printf("stream-rknn: decode_ms=%.2f ", (infer_start - decode_start) * 1000.0);
                print_result(inferences, frame.id, frame.sequence, (infer_end - infer_start) * 1000.0, &results);
            }
            free(image.virt_addr);
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

    if (push_started) {
        stop_push_client(&push_client, &http_frames);
    } else if (http_started) {
        std::lock_guard<std::mutex> guard(http_frames.mutex);
        http_frames.running = false;
        http_frames.updated.notify_all();
    }
    if (http_started) {
        stop_http_server(&http_server, &http_frames);
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
